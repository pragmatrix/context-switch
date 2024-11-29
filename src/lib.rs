use anyhow::{anyhow, bail, Result};
use futures::stream;
use futures::Stream;
use serde_json as json;
use uuid::Uuid;

mod endpoint;
mod protocol;
mod server;
