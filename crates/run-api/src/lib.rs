//! The dispatch API of the runner console (#59): five routes that requisition
//! a bench box, start or cancel a job on it over SSM, terminate it, and list
//! the live fleet. Deployed as an arm64 Lambda behind an API Gateway HTTP API
//! whose JWT authorizer (the console's Cognito pool) is the entire auth
//! layer — the crate trusts its caller, and a local `cargo run` is dev-only.
//!
//! Every AWS operation sits behind the [`cloud::Cloud`] seam so all five
//! routes are testable with a fake and no AWS in the loop — the same idea as
//! runner-live's `LogSource`. The scripts a box actually executes are built
//! by [`userdata`] (boot) and [`commands`] (SSM) as pure functions, so the
//! exact bytes that will run on a box are pinned by unit tests here.

pub mod cloud;
pub mod commands;
pub mod config;
pub mod jobs;
pub mod server;
pub mod userdata;
