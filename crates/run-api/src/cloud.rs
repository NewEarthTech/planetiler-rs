//! The AWS seam: everything the routes need from EC2 and SSM, behind a trait
//! so the whole API is testable with a fake. The real impl holds one client
//! pair per supported region — instance ids are region-scoped, and the two
//! regions are fixed by the job catalog, not discovered.

use std::collections::HashMap;

use anyhow::{Context, Result};
use async_trait::async_trait;
use aws_sdk_ec2::types as ec2t;
use base64::Engine;

/// The two regions boxes ever launch in (`jobs::Job::region` decides which).
pub const REGIONS: [&str; 2] = ["us-east-1", "us-west-2"];

/// The Name tag every bench box carries, laptop- and console-dispatched
/// alike. `boxes` filters on it, so a mutating route can only ever see —
/// and therefore touch — bench boxes; IAM enforces the same tag condition
/// underneath.
pub const BENCH_NAME: &str = "planetiler-bench";

#[derive(Clone)]
pub struct LaunchSpec {
    pub region: String,
    pub ami: String,
    pub instance_type: String,
    /// Fixed-name instance profile (logs + ECR pull + results PutObject).
    pub instance_profile: String,
    pub user_data: String,
    pub tags: Vec<(String, String)>,
    /// 0 = rely on instance-store NVMe (`d` types); >0 attaches a gp3 data
    /// volume provisioned like run.sh's (8000 IOPS / 500 MB/s) so disk class
    /// doesn't dominate results.
    pub data_ebs_gb: u32,
}

#[derive(Debug, Clone)]
pub struct BoxInfo {
    pub id: String,
    pub state: String,
    pub instance_type: String,
    pub launch_time: Option<String>,
    pub public_ip: Option<String>,
    pub tags: Vec<(String, String)>,
}

impl BoxInfo {
    pub fn tag(&self, key: &str) -> Option<&str> {
        self.tags
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

#[async_trait]
pub trait Cloud: Send + Sync {
    /// Canonical Ubuntu 24.04 arm64 AMI via the public SSM parameter — the
    /// same resolution run.sh does at dispatch.
    async fn resolve_ami(&self, region: &str) -> Result<String>;
    /// Launch one box; returns the instance id.
    async fn run_instance(&self, spec: &LaunchSpec) -> Result<String>;
    /// Every not-yet-gone instance in the region wearing the bench Name tag.
    async fn boxes(&self, region: &str) -> Result<Vec<BoxInfo>>;
    async fn terminate(&self, region: &str, id: &str) -> Result<()>;
    async fn tag(&self, region: &str, id: &str, tags: &[(String, String)]) -> Result<()>;
    /// AWS-RunShellScript on the box; returns the command id.
    async fn send_command(
        &self,
        region: &str,
        id: &str,
        script: &str,
        comment: &str,
    ) -> Result<String>;
}

/// The API's paths carry only an instance id, so look in both regions — two
/// DescribeInstances against a handful of boxes, and only bench-tagged
/// instances are visible at all.
pub async fn find_box(cloud: &dyn Cloud, id: &str) -> Result<Option<(String, BoxInfo)>> {
    for region in REGIONS {
        if let Some(info) = cloud.boxes(region).await?.into_iter().find(|b| b.id == id) {
            return Ok(Some((region.to_string(), info)));
        }
    }
    Ok(None)
}

struct RegionClients {
    ec2: aws_sdk_ec2::Client,
    ssm: aws_sdk_ssm::Client,
}

pub struct AwsCloud {
    clients: HashMap<&'static str, RegionClients>,
}

impl AwsCloud {
    pub async fn new() -> AwsCloud {
        let mut clients = HashMap::new();
        for region in REGIONS {
            let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
                .region(aws_config::Region::new(region))
                .load()
                .await;
            clients.insert(
                region,
                RegionClients {
                    ec2: aws_sdk_ec2::Client::new(&config),
                    ssm: aws_sdk_ssm::Client::new(&config),
                },
            );
        }
        AwsCloud { clients }
    }

    fn clients(&self, region: &str) -> Result<&RegionClients> {
        self.clients
            .get(region)
            .with_context(|| format!("unsupported region {region}"))
    }
}

const UBUNTU_AMI_PARAM: &str =
    "/aws/service/canonical/ubuntu/server/24.04/stable/current/arm64/hvm/ebs-gp3/ami-id";

#[async_trait]
impl Cloud for AwsCloud {
    async fn resolve_ami(&self, region: &str) -> Result<String> {
        let output = self
            .clients(region)?
            .ssm
            .get_parameter()
            .name(UBUNTU_AMI_PARAM)
            .send()
            .await
            .context("GetParameter (ubuntu ami)")?;
        output
            .parameter()
            .and_then(|p| p.value())
            .map(str::to_string)
            .context("ami parameter had no value")
    }

    async fn run_instance(&self, spec: &LaunchSpec) -> Result<String> {
        let mut mappings = vec![
            ec2t::BlockDeviceMapping::builder()
                .device_name("/dev/sda1")
                .ebs(
                    ec2t::EbsBlockDevice::builder()
                        .volume_size(30)
                        .volume_type(ec2t::VolumeType::Gp3)
                        .delete_on_termination(true)
                        .build(),
                )
                .build(),
        ];
        if spec.data_ebs_gb > 0 {
            mappings.push(
                ec2t::BlockDeviceMapping::builder()
                    .device_name("/dev/sdb")
                    .ebs(
                        ec2t::EbsBlockDevice::builder()
                            .volume_size(spec.data_ebs_gb as i32)
                            .volume_type(ec2t::VolumeType::Gp3)
                            .iops(8000)
                            .throughput(500)
                            .delete_on_termination(true)
                            .build(),
                    )
                    .build(),
            );
        }
        let tags = spec
            .tags
            .iter()
            .map(|(k, v)| ec2t::Tag::builder().key(k).value(v).build())
            .collect();
        // No key pair and no security group: the box gets the VPC default SG
        // (no ingress from anywhere outside it) and is reachable only through
        // SSM — the zero-inbound posture #59 argues for.
        let output = self
            .clients(&spec.region)?
            .ec2
            .run_instances()
            .image_id(&spec.ami)
            .instance_type(ec2t::InstanceType::from(spec.instance_type.as_str()))
            .min_count(1)
            .max_count(1)
            .instance_initiated_shutdown_behavior(ec2t::ShutdownBehavior::Terminate)
            .iam_instance_profile(
                ec2t::IamInstanceProfileSpecification::builder()
                    .name(&spec.instance_profile)
                    .build(),
            )
            .metadata_options(
                ec2t::InstanceMetadataOptionsRequest::builder()
                    .http_tokens(ec2t::HttpTokensState::Required)
                    .build(),
            )
            .set_block_device_mappings(Some(mappings))
            .tag_specifications(
                ec2t::TagSpecification::builder()
                    .resource_type(ec2t::ResourceType::Instance)
                    .set_tags(Some(tags))
                    .build(),
            )
            .user_data(base64::engine::general_purpose::STANDARD.encode(&spec.user_data))
            .send()
            .await
            .context("RunInstances")?;
        output
            .instances()
            .first()
            .and_then(|i| i.instance_id())
            .map(str::to_string)
            .context("RunInstances returned no instance id")
    }

    async fn boxes(&self, region: &str) -> Result<Vec<BoxInfo>> {
        let output = self
            .clients(region)?
            .ec2
            .describe_instances()
            .filters(
                ec2t::Filter::builder()
                    .name("tag:Name")
                    .values(BENCH_NAME)
                    .build(),
            )
            .filters(
                ec2t::Filter::builder()
                    .name("instance-state-name")
                    .values("pending")
                    .values("running")
                    .values("shutting-down")
                    .values("stopping")
                    .values("stopped")
                    .build(),
            )
            .send()
            .await
            .context("DescribeInstances")?;
        let mut boxes = Vec::new();
        for reservation in output.reservations() {
            for instance in reservation.instances() {
                boxes.push(BoxInfo {
                    id: instance.instance_id().unwrap_or_default().to_string(),
                    state: instance
                        .state()
                        .and_then(|s| s.name())
                        .map(|n| n.as_str().to_string())
                        .unwrap_or_default(),
                    instance_type: instance
                        .instance_type()
                        .map(|t| t.as_str().to_string())
                        .unwrap_or_default(),
                    launch_time: instance
                        .launch_time()
                        .and_then(|t| chrono::DateTime::from_timestamp(t.secs(), 0))
                        .map(|t| t.to_rfc3339()),
                    public_ip: instance.public_ip_address().map(str::to_string),
                    tags: instance
                        .tags()
                        .iter()
                        .filter_map(|t| Some((t.key()?.to_string(), t.value()?.to_string())))
                        .collect(),
                });
            }
        }
        Ok(boxes)
    }

    async fn terminate(&self, region: &str, id: &str) -> Result<()> {
        self.clients(region)?
            .ec2
            .terminate_instances()
            .instance_ids(id)
            .send()
            .await
            .context("TerminateInstances")?;
        Ok(())
    }

    async fn tag(&self, region: &str, id: &str, tags: &[(String, String)]) -> Result<()> {
        let mut request = self.clients(region)?.ec2.create_tags().resources(id);
        for (key, value) in tags {
            request = request.tags(ec2t::Tag::builder().key(key).value(value).build());
        }
        request.send().await.context("CreateTags")?;
        Ok(())
    }

    async fn send_command(
        &self,
        region: &str,
        id: &str,
        script: &str,
        comment: &str,
    ) -> Result<String> {
        let output = self
            .clients(region)?
            .ssm
            .send_command()
            .document_name("AWS-RunShellScript")
            .instance_ids(id)
            .parameters("commands", vec![script.to_string()])
            .comment(comment)
            .send()
            .await
            .context("SendCommand")?;
        output
            .command()
            .and_then(|c| c.command_id())
            .map(str::to_string)
            .context("SendCommand returned no command id")
    }
}
