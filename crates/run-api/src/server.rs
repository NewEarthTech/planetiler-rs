//! The five routes of #59's dispatch API. No auth here on purpose: the API
//! Gateway JWT authorizer (the console's Cognito pool) fronts the deployed
//! Lambda, so by the time a request reaches this router it has already paid
//! for itself with a valid id token. Locally there is no gateway — dev only.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{delete, get, post};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::cloud::{self, Cloud, LaunchSpec, find_box};
use crate::commands::{self, Dispatch};
use crate::config::Config;
use crate::jobs::Job;
use crate::userdata::{self, Requisition};

#[derive(Clone)]
pub struct AppState {
    pub cloud: Arc<dyn Cloud>,
    pub config: Arc<Config>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(async || "ok"))
        .route("/boxes", post(requisition).get(list))
        .route("/boxes/{id}", delete(destroy))
        .route("/boxes/{id}/jobs", post(dispatch))
        .route("/boxes/{id}/cancel", post(cancel))
        .with_state(state)
}

pub struct AppError {
    status: StatusCode,
    message: String,
}

impl AppError {
    fn bad_request(message: impl Into<String>) -> AppError {
        AppError {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }
    fn not_found(message: impl Into<String>) -> AppError {
        AppError {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }
    fn conflict(message: impl Into<String>) -> AppError {
        AppError {
            status: StatusCode::CONFLICT,
            message: message.into(),
        }
    }
}

impl From<anyhow::Error> for AppError {
    fn from(err: anyhow::Error) -> AppError {
        tracing::error!(error = %format!("{err:#}"), "cloud call failed");
        AppError {
            status: StatusCode::BAD_GATEWAY,
            message: format!("cloud call failed: {err:#}"),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

const AB_NOT_DISPATCHABLE: &str = "A/B jobs need a branch-built candidate image; until image.yml builds those, dispatch them from the laptop path";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequisitionRequest {
    pub job: String,
    pub image_tag: Option<String>,
    pub instance_type: Option<String>,
    pub max_hours: Option<u32>,
    pub data_ebs_gb: Option<u32>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequisitionResponse {
    pub id: String,
    pub region: String,
    pub job: String,
    pub instance_type: String,
    pub image_tag: String,
    pub max_hours: u32,
}

/// `POST /boxes` — launch a box that provisions itself from the runner image
/// and then sits idle waiting for a dispatch. The failsafe is armed from
/// first boot, so an abandoned requisition costs at most maxHours.
async fn requisition(
    State(state): State<AppState>,
    Json(request): Json<RequisitionRequest>,
) -> Result<(StatusCode, Json<RequisitionResponse>), AppError> {
    let job = Job::parse(&request.job)
        .ok_or_else(|| AppError::bad_request(format!("unknown job {:?}", request.job)))?;
    if !job.dispatchable() {
        return Err(AppError::conflict(AB_NOT_DISPATCHABLE));
    }
    let image_tag = request.image_tag.unwrap_or_else(|| "main".into());
    commands::check_ident("imageTag", &image_tag).map_err(AppError::bad_request)?;
    let instance_type = request
        .instance_type
        .unwrap_or_else(|| job.default_instance_type().into());
    commands::check_ident("instanceType", &instance_type).map_err(AppError::bad_request)?;
    let max_hours = request.max_hours.unwrap_or_else(|| job.default_max_hours());
    if !(1..=24).contains(&max_hours) {
        return Err(AppError::bad_request("maxHours must be 1-24"));
    }
    let data_ebs_gb = request.data_ebs_gb.unwrap_or(0);
    if data_ebs_gb > 2000 {
        return Err(AppError::bad_request("dataEbsGb must be at most 2000"));
    }

    let region = job.region();
    let ami = state.cloud.resolve_ami(region).await?;
    let user_data = userdata::requisition(&Requisition {
        max_hours,
        image: &state.config.image_uri(&image_tag),
        registry: &state.config.registry,
        repo_region: &state.config.repo_region,
    });
    let id = state
        .cloud
        .run_instance(&LaunchSpec {
            region: region.into(),
            ami,
            instance_type: instance_type.clone(),
            instance_profile: state.config.instance_profile.clone(),
            user_data,
            tags: vec![
                ("Name".into(), cloud::BENCH_NAME.into()),
                ("job".into(), job.name().into()),
                ("dispatch".into(), "console".into()),
                ("image".into(), image_tag.clone()),
                // The failsafe ceiling actually armed in user-data, so the
                // box list shows the real clock, not a form default.
                ("max-hours".into(), max_hours.to_string()),
            ],
            data_ebs_gb,
        })
        .await?;
    tracing::info!(%id, region, job = job.name(), "requisitioned");
    Ok((
        StatusCode::CREATED,
        Json(RequisitionResponse {
            id,
            region: region.into(),
            job: job.name().into(),
            instance_type,
            image_tag,
            max_hours,
        }),
    ))
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DispatchRequest {
    /// Report label for the run; defaults to the image tag the box pulled.
    #[serde(rename = "ref")]
    pub ref_label: Option<String>,
    pub planet_url: Option<String>,
    pub max_hours: Option<u32>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchResponse {
    pub run_id: String,
    pub command_id: String,
    pub events_url: String,
}

/// `POST /boxes/{id}/jobs` — start the box's job over SSM. Re-dispatching
/// onto a kept box works like redispatch.sh: previous job cleared, staged
/// input on /data reused, failsafe clock reset.
async fn dispatch(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<DispatchRequest>>,
) -> Result<(StatusCode, Json<DispatchResponse>), AppError> {
    let request = body.map(|Json(b)| b).unwrap_or_default();
    let (region, info) = find_box(&*state.cloud, &id)
        .await?
        .ok_or_else(|| AppError::not_found(format!("no bench box {id}")))?;
    // The SSM script waits on /home/ubuntu/.box-ready, which only console
    // requisition writes — a laptop box would take the 202 and then fail on
    // that wait ~15 minutes later, so refuse it here instead.
    if info.tag("dispatch") != Some("console") {
        return Err(AppError::conflict(
            "box was not requisitioned by the console; dispatch it from the laptop path",
        ));
    }
    let job = info
        .tag("job")
        .and_then(Job::parse)
        .ok_or_else(|| AppError::conflict("box carries no known job tag"))?;
    if !job.dispatchable() {
        return Err(AppError::conflict(AB_NOT_DISPATCHABLE));
    }
    let ref_label = request
        .ref_label
        .or_else(|| info.tag("image").map(str::to_string))
        .unwrap_or_else(|| "main".into());
    commands::check_ident("ref", &ref_label).map_err(AppError::bad_request)?;
    let planet_url = request
        .planet_url
        .unwrap_or_else(|| state.config.planet_url.clone());
    commands::check_url("planetUrl", &planet_url).map_err(AppError::bad_request)?;
    let max_hours = request.max_hours.unwrap_or_else(|| job.default_max_hours());
    if !(1..=24).contains(&max_hours) {
        return Err(AppError::bad_request("maxHours must be 1-24"));
    }

    let run_id = commands::new_run_id(job.name(), time::OffsetDateTime::now_utc());
    let script = commands::dispatch_script(&Dispatch {
        run_id: &run_id,
        ref_label: &ref_label,
        planet_url: &planet_url,
        max_hours,
        drain_group: &state.config.drain_group,
        drain_region: &state.config.drain_region,
    });
    let command_id = state
        .cloud
        .send_command(
            &region,
            &id,
            &script,
            &format!("planetiler-rs dispatch {run_id}"),
        )
        .await?;
    // The run-id tag is how the box list shows what a box is doing; the
    // dispatch script just re-armed the failsafe, so the ceiling moves too.
    state
        .cloud
        .tag(
            &region,
            &id,
            &[
                ("run-id".into(), run_id.clone()),
                ("max-hours".into(), max_hours.to_string()),
            ],
        )
        .await?;
    tracing::info!(%id, region, run_id, "dispatched");
    Ok((
        StatusCode::ACCEPTED,
        Json(DispatchResponse {
            events_url: state.config.events_url(&run_id),
            run_id,
            command_id,
        }),
    ))
}

/// `POST /boxes/{id}/cancel` — kill the running job; the box stays up for
/// inspection or redispatch (the boot failsafe still reaps it).
async fn cancel(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (region, _info) = find_box(&*state.cloud, &id)
        .await?
        .ok_or_else(|| AppError::not_found(format!("no bench box {id}")))?;
    let command_id = state
        .cloud
        .send_command(
            &region,
            &id,
            &commands::cancel_script(),
            "planetiler-rs cancel",
        )
        .await?;
    tracing::info!(%id, region, "cancelled");
    Ok(Json(json!({ "commandId": command_id })))
}

/// `DELETE /boxes/{id}` — terminate. Only bench-tagged boxes are findable,
/// so this cannot touch anything else even before IAM says the same.
async fn destroy(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (region, _info) = find_box(&*state.cloud, &id)
        .await?
        .ok_or_else(|| AppError::not_found(format!("no bench box {id}")))?;
    state.cloud.terminate(&region, &id).await?;
    tracing::info!(%id, region, "terminating");
    Ok(Json(
        json!({ "id": id, "region": region, "state": "terminating" }),
    ))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BoxView {
    pub id: String,
    pub region: String,
    pub state: String,
    pub instance_type: String,
    pub launch_time: Option<String>,
    pub public_ip: Option<String>,
    pub job: Option<String>,
    pub image: Option<String>,
    pub run_id: Option<String>,
    /// "console" for boxes this API launched; absent on laptop dispatches.
    pub dispatch: Option<String>,
    /// UTC stamp the box wrote once requisition finished (.box-ready). Absent
    /// while provisioning is still running — EC2 `running` is not readiness.
    pub ready: Option<String>,
    /// The failsafe ceiling currently armed on the box, in hours.
    pub max_hours: Option<u32>,
    pub events_url: Option<String>,
}

/// `GET /boxes` — the live fleet across both regions, laptop-dispatched
/// boxes included, with everything the console needs to attach: no separate
/// discovery machinery.
async fn list(State(state): State<AppState>) -> Result<Json<serde_json::Value>, AppError> {
    let mut boxes = Vec::new();
    for region in cloud::REGIONS {
        for info in state.cloud.boxes(region).await? {
            boxes.push(BoxView {
                job: info.tag("job").map(str::to_string),
                image: info.tag("image").map(str::to_string),
                run_id: info.tag("run-id").map(str::to_string),
                dispatch: info.tag("dispatch").map(str::to_string),
                ready: info.tag("ready").map(str::to_string),
                max_hours: info.tag("max-hours").and_then(|h| h.parse().ok()),
                events_url: info.tag("run-id").map(|run| state.config.events_url(run)),
                id: info.id,
                region: region.into(),
                state: info.state,
                instance_type: info.instance_type,
                launch_time: info.launch_time,
                public_ip: info.public_ip,
            });
        }
    }
    Ok(Json(json!({ "boxes": boxes })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::BoxInfo;
    use anyhow::Result;
    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use std::sync::Mutex;
    use tower::util::ServiceExt;

    /// (region, id, tags) per CreateTags call.
    type TagCall = (String, String, Vec<(String, String)>);

    /// Records every mutation; only bench-tagged seeds are visible, the same
    /// way the real `boxes` only sees the Name-tag filter's matches.
    #[derive(Default)]
    struct FakeCloud {
        seeded: Mutex<Vec<(String, BoxInfo)>>,
        launches: Mutex<Vec<LaunchSpec>>,
        commands: Mutex<Vec<(String, String, String)>>,
        tagged: Mutex<Vec<TagCall>>,
        terminated: Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl Cloud for FakeCloud {
        async fn resolve_ami(&self, _region: &str) -> Result<String> {
            Ok("ami-test".into())
        }
        async fn run_instance(&self, spec: &LaunchSpec) -> Result<String> {
            self.launches.lock().unwrap().push(spec.clone());
            Ok("i-0new".into())
        }
        async fn boxes(&self, region: &str) -> Result<Vec<BoxInfo>> {
            Ok(self
                .seeded
                .lock()
                .unwrap()
                .iter()
                .filter(|(r, b)| r == region && b.tag("Name") == Some(cloud::BENCH_NAME))
                .map(|(_, b)| b.clone())
                .collect())
        }
        async fn terminate(&self, region: &str, id: &str) -> Result<()> {
            self.terminated
                .lock()
                .unwrap()
                .push((region.into(), id.into()));
            Ok(())
        }
        async fn tag(&self, region: &str, id: &str, tags: &[(String, String)]) -> Result<()> {
            self.tagged
                .lock()
                .unwrap()
                .push((region.into(), id.into(), tags.to_vec()));
            Ok(())
        }
        async fn send_command(
            &self,
            region: &str,
            id: &str,
            script: &str,
            _comment: &str,
        ) -> Result<String> {
            self.commands
                .lock()
                .unwrap()
                .push((region.into(), id.into(), script.into()));
            Ok("cmd-1".into())
        }
    }

    fn state(fake: Arc<FakeCloud>) -> AppState {
        AppState {
            cloud: fake,
            config: Arc::new(
                Config::new(
                    "123.dkr.ecr.us-east-1.amazonaws.com/planetiler-rs-bench".into(),
                    "https://live.runner.example".into(),
                    "/planetiler-rs/runs".into(),
                    "us-east-1".into(),
                    "planetiler-rs-bench-box".into(),
                    "https://planet.example/planet.osm.pbf".into(),
                )
                .unwrap(),
            ),
        }
    }

    fn bench_box(id: &str, tags: &[(&str, &str)]) -> BoxInfo {
        BoxInfo {
            id: id.into(),
            state: "running".into(),
            instance_type: "r7gd.8xlarge".into(),
            launch_time: Some("2026-07-24T10:00:00+00:00".into()),
            public_ip: None,
            tags: tags
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    fn planet_tags() -> Vec<(&'static str, &'static str)> {
        vec![
            ("Name", cloud::BENCH_NAME),
            ("job", "planet"),
            ("dispatch", "console"),
            ("image", "main"),
        ]
    }

    async fn call(
        app: &Router,
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, serde_json::Value) {
        let builder = Request::builder().method(method).uri(uri);
        let request = match body {
            Some(value) => builder
                .header("content-type", "application/json")
                .body(Body::from(value.to_string()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| json!(String::from_utf8_lossy(&bytes)));
        (status, value)
    }

    #[tokio::test]
    async fn requisition_launches_a_planet_box_in_us_west_2() {
        let fake = Arc::new(FakeCloud::default());
        let app = router(state(fake.clone()));
        let (status, body) = call(&app, "POST", "/boxes", Some(json!({"job": "planet"}))).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["id"], "i-0new");
        assert_eq!(body["region"], "us-west-2");
        assert_eq!(body["instanceType"], "r7gd.8xlarge");
        assert_eq!(body["maxHours"], 14);
        let launches = fake.launches.lock().unwrap();
        let spec = &launches[0];
        assert_eq!(spec.region, "us-west-2");
        assert_eq!(spec.ami, "ami-test");
        assert_eq!(spec.data_ebs_gb, 0);
        assert!(spec.user_data.contains("shutdown -h +840"));
        assert!(spec.user_data.contains("planetiler-rs-bench:main"));
        assert!(spec.tags.contains(&("dispatch".into(), "console".into())));
        assert!(spec.tags.contains(&("max-hours".into(), "14".into())));
        assert!(
            spec.tags
                .contains(&("Name".into(), cloud::BENCH_NAME.into()))
        );
    }

    #[tokio::test]
    async fn requisition_rejects_ab_jobs_and_hostile_input() {
        let app = router(state(Arc::new(FakeCloud::default())));
        let (status, body) = call(&app, "POST", "/boxes", Some(json!({"job": "gb-ab"}))).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body["error"].as_str().unwrap().contains("branch-built"));
        let (status, _) = call(&app, "POST", "/boxes", Some(json!({"job": "nonsense"}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = call(
            &app,
            "POST",
            "/boxes",
            Some(json!({"job": "planet", "imageTag": "$(id)"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = call(
            &app,
            "POST",
            "/boxes",
            Some(json!({"job": "planet", "maxHours": 0})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn dispatch_starts_the_job_and_tags_the_run() {
        let fake = Arc::new(FakeCloud::default());
        fake.seeded
            .lock()
            .unwrap()
            .push(("us-west-2".into(), bench_box("i-0abc", &planet_tags())));
        let app = router(state(fake.clone()));
        let (status, body) = call(&app, "POST", "/boxes/i-0abc/jobs", None).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        let run_id = body["runId"].as_str().unwrap();
        assert!(run_id.starts_with("planet-"), "{run_id}");
        assert_eq!(
            body["eventsUrl"],
            format!("https://live.runner.example/runs/{run_id}/events")
        );
        let commands = fake.commands.lock().unwrap();
        let (region, id, script) = &commands[0];
        assert_eq!((region.as_str(), id.as_str()), ("us-west-2", "i-0abc"));
        // ref defaults to the image tag the box pulled; the URL is the pin.
        assert!(script.contains(&format!(
            "env RUN_ID={run_id} PTRS_BIN=/home/ubuntu/ptrs ./remote-planet.sh main https://planet.example/planet.osm.pbf"
        )));
        assert!(script.contains(&format!("./drain.sh {run_id}")));
        let tagged = fake.tagged.lock().unwrap();
        assert_eq!(
            tagged[0].2,
            vec![
                ("run-id".to_string(), run_id.to_string()),
                ("max-hours".to_string(), "14".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn dispatch_is_404_without_a_box_and_409_for_ab_boxes() {
        let fake = Arc::new(FakeCloud::default());
        fake.seeded.lock().unwrap().push((
            "us-east-1".into(),
            bench_box(
                "i-0gb",
                &[
                    ("Name", cloud::BENCH_NAME),
                    ("job", "gb-ab"),
                    ("dispatch", "console"),
                ],
            ),
        ));
        let app = router(state(fake.clone()));
        let (status, _) = call(&app, "POST", "/boxes/i-0nope/jobs", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = call(&app, "POST", "/boxes/i-0gb/jobs", None).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(fake.commands.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn dispatch_refuses_boxes_the_console_did_not_requisition() {
        let fake = Arc::new(FakeCloud::default());
        // A laptop box: right job, but no dispatch tag — its requisition
        // never writes .box-ready, so the SSM script would hang and fail.
        fake.seeded.lock().unwrap().push((
            "us-west-2".into(),
            bench_box(
                "i-0laptop",
                &[("Name", cloud::BENCH_NAME), ("job", "planet")],
            ),
        ));
        let app = router(state(fake.clone()));
        let (status, body) = call(&app, "POST", "/boxes/i-0laptop/jobs", None).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body["error"].as_str().unwrap().contains("laptop path"));
        assert!(fake.commands.lock().unwrap().is_empty());
        assert!(fake.tagged.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn cancel_reaches_the_box_and_keeps_it() {
        let fake = Arc::new(FakeCloud::default());
        fake.seeded
            .lock()
            .unwrap()
            .push(("us-west-2".into(), bench_box("i-0abc", &planet_tags())));
        let app = router(state(fake.clone()));
        let (status, body) = call(&app, "POST", "/boxes/i-0abc/cancel", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["commandId"], "cmd-1");
        let commands = fake.commands.lock().unwrap();
        assert!(commands[0].2.contains("pkill -f \"/home/ubuntu/ptrs\""));
        assert!(fake.terminated.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn destroy_terminates_bench_boxes_only() {
        let fake = Arc::new(FakeCloud::default());
        fake.seeded
            .lock()
            .unwrap()
            .push(("us-west-2".into(), bench_box("i-0abc", &planet_tags())));
        // Wrong Name tag: invisible to the API, exactly like the real filter.
        fake.seeded.lock().unwrap().push((
            "us-east-1".into(),
            bench_box("i-0other", &[("Name", "web-server")]),
        ));
        let app = router(state(fake.clone()));
        let (status, _) = call(&app, "DELETE", "/boxes/i-0abc", None).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = call(&app, "DELETE", "/boxes/i-0other", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            *fake.terminated.lock().unwrap(),
            vec![("us-west-2".to_string(), "i-0abc".to_string())]
        );
    }

    #[tokio::test]
    async fn list_aggregates_both_regions_with_events_urls() {
        let fake = Arc::new(FakeCloud::default());
        let mut running = planet_tags();
        running.push(("run-id", "planet-20260724-101112"));
        running.push(("ready", "2026-07-24T10:01:07Z"));
        running.push(("max-hours", "14"));
        fake.seeded
            .lock()
            .unwrap()
            .push(("us-west-2".into(), bench_box("i-0planet", &running)));
        // A laptop-dispatched extract box: bench Name tag, no dispatch/run-id.
        fake.seeded.lock().unwrap().push((
            "us-east-1".into(),
            bench_box("i-0gb", &[("Name", cloud::BENCH_NAME), ("job", "gb-ab")]),
        ));
        let app = router(state(fake));
        let (status, body) = call(&app, "GET", "/boxes", None).await;
        assert_eq!(status, StatusCode::OK);
        let boxes = body["boxes"].as_array().unwrap();
        assert_eq!(boxes.len(), 2);
        let planet = boxes.iter().find(|b| b["id"] == "i-0planet").unwrap();
        assert_eq!(planet["region"], "us-west-2");
        assert_eq!(planet["dispatch"], "console");
        assert_eq!(planet["ready"], "2026-07-24T10:01:07Z");
        assert_eq!(planet["maxHours"], 14);
        assert_eq!(
            planet["eventsUrl"],
            "https://live.runner.example/runs/planet-20260724-101112/events"
        );
        let gb = boxes.iter().find(|b| b["id"] == "i-0gb").unwrap();
        assert_eq!(gb["eventsUrl"], serde_json::Value::Null);
        assert_eq!(gb["dispatch"], serde_json::Value::Null);
        assert_eq!(gb["ready"], serde_json::Value::Null);
        assert_eq!(gb["maxHours"], serde_json::Value::Null);
    }
}
