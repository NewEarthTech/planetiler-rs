//! The SSM Run Command scripts for the dispatch and cancel legs, plus the
//! input validation that makes embedding values in them safe. Everything a
//! caller can influence must pass [`check_ident`] or [`check_url`] first — a
//! strict allowlist, not an escape — so the generated scripts only ever
//! contain characters that mean themselves to a shell.

use time::OffsetDateTime;
use time::macros::format_description;

/// Tags, refs, image tags, instance types: `[A-Za-z0-9._/:-]`, non-empty,
/// bounded. Nothing a shell or an EC2 tag value would reinterpret.
pub fn check_ident(what: &str, value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > 128 {
        return Err(format!("{what} must be 1-128 characters"));
    }
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | ':'))
    {
        Ok(())
    } else {
        Err(format!("{what} may only contain [A-Za-z0-9._/:-]"))
    }
}

/// The planet input URL: https, and only URL-shaped bytes — nothing that
/// means anything inside the double-quote-free scripts below.
pub fn check_url(what: &str, value: &str) -> Result<(), String> {
    if !value.starts_with("https://") || value.len() > 512 {
        return Err(format!("{what} must be an https URL under 512 chars"));
    }
    if value.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(
                c,
                '.' | '_' | '~' | ':' | '/' | '?' | '#' | '&' | '=' | '%' | '+' | '-'
            )
    }) {
        Ok(())
    } else {
        Err(format!(
            "{what} contains characters outside the URL allowlist"
        ))
    }
}

/// Same shape run.sh uses, so a run is one id everywhere: the drain streams,
/// the S3 results prefix, the console's run picker.
pub fn new_run_id(job: &str, now: OffsetDateTime) -> String {
    let stamp = now
        .format(format_description!(
            "[year][month][day]-[hour][minute][second]"
        ))
        .expect("fixed format on a valid datetime");
    format!("{job}-{stamp}")
}

pub struct Dispatch<'a> {
    pub run_id: &'a str,
    /// Report label for the run — the image tag/sha for a console dispatch.
    pub ref_label: &'a str,
    pub planet_url: &'a str,
    pub max_hours: u32,
    pub drain_group: &'a str,
    pub drain_region: &'a str,
}

/// Start a planet run through the box's shared entry point (`run-job.sh`, the
/// same command `just run planet` reaches on a laptop — it sees IMDS answer
/// and hands off to remote-planet.sh). The pinned input URL still travels
/// from here, so the reproducibility pin stays server-side rather than
/// riding the image. Wait out requisition, clear any previous job the way
/// redispatch.sh does, re-arm the failsafe, then start the drain and the job
/// as systemd transient units. NOT nohup: the SSM agent tracks every process
/// the invocation spawns and SIGKILLs whatever is left at executionTimeout —
/// the first console planet run died to the second, 3600 s after dispatch,
/// traps unfired (status RUNNING forever, nothing shipped). A transient unit
/// lives outside the agent's tree, so the invocation completes in seconds
/// and there is nothing left to reap. Unit names carry the run id: the
/// reset's pkill has killed the previous main process, but its unit may not
/// be garbage-collected yet. PATH gains /snap/bin explicitly — systemd units
/// don't inherit the agent's snap-friendly PATH, and aws lives there.
pub fn dispatch_script(dispatch: &Dispatch) -> String {
    format!(
        r#"#!/bin/bash
set -u
# Requisition (image pull, script extraction) may still be running on a
# fresh box; its last act is the .box-ready marker.
for _ in $(seq 1 180); do [ -f /home/ubuntu/.box-ready ] && break; sleep 5; done
[ -f /home/ubuntu/.box-ready ] || {{ echo "box never finished requisition (no .box-ready)" >&2; exit 1; }}
# Reset any previous job + the failsafe clock (redispatch.sh's reset, sans
# ssh). /data is deliberately untouched: a redispatch reuses the staged input.
# results/ is cleared: the previous run shipped its own results at its EXIT,
# and anything left here would ship again under THIS run's S3 prefix (seen
# live on the first smoke redispatch).
pkill -f "\./run-job\.sh" 2>/dev/null; pkill -f "\./remote\.sh" 2>/dev/null; pkill -f "\./remote-planet\.sh" 2>/dev/null; pkill -f "\./drain\.sh" 2>/dev/null
sleep 2
rm -rf /home/ubuntu/status /home/ubuntu/job.log /home/ubuntu/repo /home/ubuntu/planet-mem.log /home/ubuntu/planet-rs.stderr /home/ubuntu/drain.log /home/ubuntu/results
shutdown -c 2>/dev/null || true
shutdown -h +{minutes}
systemd-run --collect --unit "ptrs-drain-{run_id}" --uid ubuntu --gid ubuntu --working-directory /home/ubuntu -- bash -c 'exec env PATH="$PATH:/snap/bin" DRAIN_GROUP={drain_group} DRAIN_REGION={drain_region} DRAIN_POLL=1 ./drain.sh {run_id} > drain.log 2>&1'
systemd-run --collect --unit "ptrs-job-{run_id}" --uid ubuntu --gid ubuntu --working-directory /home/ubuntu -- bash -c 'exec env PATH="$PATH:/snap/bin" RUN_ID={run_id} PTRS_BIN=/home/ubuntu/ptrs PLANET_URL={planet_url} ./run-job.sh planet {ref_label} > job.log 2>&1'
echo "dispatched {run_id}"
"#,
        minutes = dispatch.max_hours * 60,
        drain_group = dispatch.drain_group,
        drain_region = dispatch.drain_region,
        run_id = dispatch.run_id,
        ref_label = dispatch.ref_label,
        planet_url = dispatch.planet_url,
    )
}

/// Cancel the running job but keep the box (the failsafe still reaps it).
/// The binary dies first: remote-planet.sh's `wait` then returns and the
/// script concludes through its normal recorded-failure path — status
/// written, report lines said, results shipped to S3. The script pkills
/// mop up a cancel that lands before the run starts (download, deps).
pub fn cancel_script() -> String {
    r#"#!/bin/bash
set -u
pkill -f "/home/ubuntu/ptrs" 2>/dev/null
pkill -f "planetiler-.*\.jar" 2>/dev/null
sleep 3
pkill -f "\./remote-planet\.sh" 2>/dev/null
pkill -f "\./remote\.sh" 2>/dev/null
pkill -f "\./run-job\.sh" 2>/dev/null
# drain.sh exits on its own once status reads DONE/FAIL and it is caught up.
echo cancelled
"#
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idents_admit_the_real_values_and_reject_shell() {
        for ok in [
            "main",
            "r7gd.8xlarge",
            "planet-20260724-101112",
            "5bb72be",
            "perf/foo-bar",
        ] {
            assert!(check_ident("x", ok).is_ok(), "{ok} should pass");
        }
        for bad in [
            "",
            "a b",
            "a;b",
            "$(id)",
            "a`b`",
            "a'b",
            "a\"b",
            "a\nb",
            &"x".repeat(129),
        ] {
            assert!(check_ident("x", bad).is_err(), "{bad:?} should fail");
        }
    }

    #[test]
    fn urls_admit_the_pinned_planet_and_reject_everything_shaped_wrong() {
        assert!(
            check_url(
                "x",
                "https://osm-planet-us-west-2.s3.dualstack.us-west-2.amazonaws.com/planet/pbf/2026/planet-260713.osm.pbf"
            )
            .is_ok()
        );
        for bad in [
            "http://plain.example/x",
            "https://a b",
            "https://a;b",
            "https://$(id)",
        ] {
            assert!(check_url("x", bad).is_err(), "{bad:?} should fail");
        }
    }

    #[test]
    fn run_ids_match_the_run_sh_shape() {
        let now = time::macros::datetime!(2026-07-24 10:11:12 UTC);
        assert_eq!(new_run_id("planet", now), "planet-20260724-101112");
    }

    #[test]
    fn dispatch_waits_resets_and_starts_drain_before_job() {
        let script = dispatch_script(&Dispatch {
            run_id: "planet-20260724-101112",
            ref_label: "5bb72be",
            planet_url: "https://example.com/planet.osm.pbf",
            max_hours: 14,
            drain_group: "/planetiler-rs/runs",
            drain_region: "us-east-1",
        });
        assert!(script.contains(".box-ready"));
        assert!(script.contains("shutdown -h +840"));
        // The reset clears the previous run's results (already shipped at its
        // exit) but never /data — the staged input is the redispatch's point.
        assert!(script.contains("/home/ubuntu/results"));
        assert!(!script.contains("rm -rf /data"));
        assert!(script.contains(
            "DRAIN_GROUP=/planetiler-rs/runs DRAIN_REGION=us-east-1 DRAIN_POLL=1 ./drain.sh planet-20260724-101112"
        ));
        // One entry point for both sides of the fence: the box runs the same
        // `run-job.sh <job> <ref>` a laptop does, with the pinned URL passed
        // in so the catalogue resolves to exactly this input.
        assert!(script.contains(
            "RUN_ID=planet-20260724-101112 PTRS_BIN=/home/ubuntu/ptrs PLANET_URL=https://example.com/planet.osm.pbf ./run-job.sh planet 5bb72be"
        ));
        // Drain first, so job.log ships from its first line; both detached.
        assert!(script.find("drain.sh").unwrap() < script.find("run-job.sh").unwrap());
        // systemd units, never nohup: the SSM agent SIGKILLs everything left
        // in the invocation's tree at executionTimeout (3600 s default) —
        // this exact script's processes died to the second on the first
        // planet proof run. Run-id-suffixed names so a redispatch can't
        // collide with a not-yet-collected unit.
        assert!(!script.contains("nohup"));
        assert_eq!(script.matches("systemd-run --collect").count(), 2);
        assert!(script.contains(r#"--unit "ptrs-drain-planet-20260724-101112""#));
        assert!(script.contains(r#"--unit "ptrs-job-planet-20260724-101112""#));
        // systemd PATH has no /snap/bin, and aws (drain + results ship)
        // lives there on Ubuntu.
        assert_eq!(script.matches(r#"PATH="$PATH:/snap/bin""#).count(), 2);
    }

    #[test]
    fn cancel_kills_the_binary_before_the_script() {
        let script = cancel_script();
        assert!(script.find("/home/ubuntu/ptrs").unwrap() < script.find("remote-planet").unwrap());
        assert!(script.find("/home/ubuntu/ptrs").unwrap() < script.find("run-job").unwrap());
        assert!(script.contains("planetiler-.*\\.jar"));
    }
}
