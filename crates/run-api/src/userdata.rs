//! Builds the user-data a console-requisitioned box boots with. The laptop
//! path (`run.sh`) arms the failsafe and mounts /data in user-data and ships
//! everything else over ssh; with no ssh, requisition also has to land the
//! tooling and the job material — aws + docker, the runner image, and the
//! box-side scripts + CLI lifted out of it. The failsafe and mount blocks are
//! verbatim run.sh so both dispatch paths produce the same disk behavior.

pub struct Requisition<'a> {
    pub max_hours: u32,
    /// Full image URI with tag.
    pub image: &'a str,
    /// Registry host, the `docker login` target.
    pub registry: &'a str,
    /// Region the registry lives in.
    pub repo_region: &'a str,
}

/// The boot script, run as root by cloud-init. If any stage fails —
/// no mountable /data disk, a bad image tag, ECR auth — `.box-ready` never
/// appears and dispatch fails loudly on its readiness wait instead of
/// starting a job on a half-provisioned box (the failsafe still reaps it).
pub fn requisition(req: &Requisition) -> String {
    format!(
        r#"#!/bin/bash
shutdown -h +{minutes}
# /data: prefer local instance-store NVMe; else the attached data EBS volume
# (the disk with no partitions and nothing mounted).
dev=$(lsblk -dno NAME,MODEL | awk "/Instance Storage/{{print \"/dev/\"\$1; exit}}")
if [ ! -b "$dev" ]; then
  for d in $(lsblk -pdno NAME,TYPE | awk "\$2==\"disk\"{{print \$1}}"); do
    [ -z "$(lsblk -no MOUNTPOINTS "$d" | tr -d "[:space:]")" ] && dev=$d && break
  done
fi
[ -b "$dev" ] || exit 1
mkfs.ext4 -F "$dev"
mkdir -p /data && mount "$dev" /data && chown ubuntu:ubuntu /data
# Console-dispatch tooling: aws via snap (same as drain.sh), docker to pull
# the runner image. The scripts run on the HOST — the container is only the
# delivery vehicle for bytes that version with the image sha.
#
# Every stage from here is load-bearing, so fail fast: the readiness marker
# must only ever be written by a box that actually holds the job material.
# pipefail is what makes the ECR login pipe honest — without it a failed
# credential fetch is masked by the exit code of what it feeds. The mount
# block above deliberately stays outside this: its disk-probe loop ends on a
# false test in the common case, which set -e would turn into an exit.
set -eo pipefail
export DEBIAN_FRONTEND=noninteractive
snap install aws-cli --classic
# snap links binaries into /snap/bin, which reaches shells through a
# /etc/profile.d entry — cloud-init runs this as root in a non-login shell,
# so PATH has to pick it up explicitly or every aws call below is a
# command-not-found. drain.sh guards the same hazard with command -v.
export PATH="$PATH:/snap/bin"
command -v aws >/dev/null || {{ echo "aws cli missing after snap install" >&2; exit 1; }}
apt-get update -qq
apt-get install -y -qq docker.io
aws ecr get-login-password --region {repo_region} | docker login --username AWS --password-stdin {registry}
docker pull -q {image}
c=$(docker create {image})
docker cp "$c:/opt/cloud-runner/." /home/ubuntu/
docker cp "$c:/usr/local/bin/planetiler-rs" /home/ubuntu/ptrs
docker rm "$c" >/dev/null
chmod +x /home/ubuntu/*.sh /home/ubuntu/ptrs
chown ubuntu:ubuntu /home/ubuntu/*.sh /home/ubuntu/ptrs
touch /home/ubuntu/.box-ready && chown ubuntu:ubuntu /home/ubuntu/.box-ready
"#,
        minutes = req.max_hours * 60,
        repo_region = req.repo_region,
        registry = req.registry,
        image = req.image,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script() -> String {
        requisition(&Requisition {
            max_hours: 14,
            image: "123.dkr.ecr.us-east-1.amazonaws.com/planetiler-rs-bench:main",
            registry: "123.dkr.ecr.us-east-1.amazonaws.com",
            repo_region: "us-east-1",
        })
    }

    #[test]
    fn arms_the_failsafe_in_minutes() {
        assert!(script().contains("shutdown -h +840\n"));
    }

    #[test]
    fn keeps_the_run_sh_mount_block_verbatim() {
        let script = script();
        // The awk escapes are the exact bytes run.sh ships; a drift here means
        // the two dispatch paths no longer format disks the same way.
        assert!(script.contains(r#"awk "/Instance Storage/{print \"/dev/\"\$1; exit}""#));
        assert!(script.contains(r#"[ -b "$dev" ] || exit 1"#));
        assert!(
            script.contains(r#"mkdir -p /data && mount "$dev" /data && chown ubuntu:ubuntu /data"#)
        );
    }

    #[test]
    fn pulls_the_image_and_extracts_the_job_material() {
        let script = script();
        assert!(script.contains(
            "docker pull -q 123.dkr.ecr.us-east-1.amazonaws.com/planetiler-rs-bench:main"
        ));
        assert!(script.contains(r#"docker cp "$c:/opt/cloud-runner/." /home/ubuntu/"#));
        assert!(
            script.contains(r#"docker cp "$c:/usr/local/bin/planetiler-rs" /home/ubuntu/ptrs"#)
        );
        // Readiness marker lands last, so dispatch can wait on it.
        let ready = script.find(".box-ready").unwrap();
        assert!(ready > script.find("docker pull").unwrap());
    }

    #[test]
    fn the_tooling_stages_are_fail_fast_before_the_ready_marker() {
        let script = script();
        // Ordering alone doesn't make .box-ready conditional — without a
        // set -e covering the stages between, a failed ECR login or image
        // pull still falls through to the marker and dispatch then starts a
        // job on a box with no scripts and no binary.
        let fail_fast = script
            .find("set -eo pipefail")
            .expect("tooling is fail-fast");
        for stage in [
            "snap install aws-cli",
            "get-login-password",
            "docker pull",
            "docker cp",
        ] {
            assert!(
                script.find(stage).unwrap() > fail_fast,
                "{stage} must run under set -e"
            );
        }
        assert!(script.find(".box-ready").unwrap() > fail_fast);
    }

    #[test]
    fn snap_bin_is_on_path_before_the_first_aws_call() {
        let script = script();
        // cloud-init's root shell is not a login shell, so the
        // /etc/profile.d entry snap relies on never runs here.
        let path = script.find(r#"export PATH="$PATH:/snap/bin""#).unwrap();
        assert!(path > script.find("snap install aws-cli").unwrap());
        assert!(path < script.find("get-login-password").unwrap());
    }
}
