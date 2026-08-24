//! Shared core for the HTTP Client CLI.
//!
//! Implements a practical subset of common `.http` request file conventions:
//!
//! * Requests separated by `###`, optional names, and request tags
//!   (`@no-redirect`, `@timeout`, `@connection-timeout`)
//! * In-place variables (`@name = value`) and `{{variable}}` substitution with
//!   the documented priority: environment > global > in-place > per-request
//! * Environment files (`http-client.env.json`, `http-client.private.env.json`)
//!   discovered from the request file's directory up to the project root;
//!   private values override public ones
//! * Dynamic variables (`{{$uuid}}`, `{{$timestamp}}`, `{{$randomInt}}`,
//!   `{{$random.integer(a,b)}}`, `{{$env.NAME}}`, ...)
//! * A response-handler subset (`client.global.set`, `client.log`) rich enough
//!   for login-then-use-token chaining workflows

pub mod env;
pub mod execute;
pub mod handler;
pub mod output;
pub mod parser;
pub mod rng;
pub mod vars;

pub use env::{Environment, PRIVATE_ENV_FILE, PUBLIC_ENV_FILE};
pub use execute::{resolve_request, ResolvedRequest, ResponseData};
pub use parser::{parse, Handler, Header, HttpFile, Redirect, Request};
pub use rng::Rng;
pub use vars::{substitute, VarContext};
