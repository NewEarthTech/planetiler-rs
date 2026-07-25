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

/// Start a planet run: wait out requisition, clear any previous job the way
/// redispatch.sh does, re-arm the failsafe, then start the drain and the job
/// exactly as the ssh path would — nohup'd, detached, owned by ubuntu. The
/// `< /dev/null` redirects are what let the SSM invocation complete while
/// the job lives on (same load-bearing trick as run.sh's ssh dispatch).
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
pkill -f "\./remote\.sh" 2>/dev/null; pkill -f "\./remote-planet\.sh" 2>/dev/null; pkill -f "\./drain\.sh" 2>/dev/null
sleep 2
rm -rf /home/ubuntu/status /home/ubuntu/job.log /home/ubuntu/repo /home/ubuntu/planet-mem.log /home/ubuntu/planet-rs.stderr /home/ubuntu/drain.log
shutdown -c 2>/dev/null || true
shutdown -h +{minutes}
runuser -u ubuntu -- bash -c 'cd && nohup env DRAIN_GROUP={drain_group} DRAIN_REGION={drain_region} DRAIN_POLL=1 ./drain.sh {run_id} > drain.log 2>&1 < /dev/null &'
runuser -u ubuntu -- bash -c 'cd && nohup env RUN_ID={run_id} PTRS_BIN=/home/ubuntu/ptrs ./remote-planet.sh {ref_label} {planet_url} > job.log 2>&1 < /dev/null &'
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
        assert!(script.contains(
            "env DRAIN_GROUP=/planetiler-rs/runs DRAIN_REGION=us-east-1 DRAIN_POLL=1 ./drain.sh planet-20260724-101112"
        ));
        assert!(script.contains(
            "env RUN_ID=planet-20260724-101112 PTRS_BIN=/home/ubuntu/ptrs ./remote-planet.sh 5bb72be https://example.com/planet.osm.pbf"
        ));
        // Drain first, so job.log ships from its first line; both detached.
        assert!(script.find("drain.sh").unwrap() < script.find("remote-planet.sh").unwrap());
        assert_eq!(script.matches("< /dev/null").count(), 2);
    }

    #[test]
    fn cancel_kills_the_binary_before_the_script() {
        let script = cancel_script();
        assert!(script.find("/home/ubuntu/ptrs").unwrap() < script.find("remote-planet").unwrap());
        assert!(script.contains("planetiler-.*\\.jar"));
    }
}
