//! CookLangHub: a collaborative Cooklang Recipe platform backed by Forgejo.
//!
//! Forgejo and Git hold the authoritative Recipe and Cookbook state. This
//! application keeps operational state only, and every piece of it is
//! rebuildable.

pub mod config;
pub mod db;
pub mod forgejo;
pub mod health;
pub mod secret;
pub mod telemetry;
pub mod web;

pub use config::Config;
