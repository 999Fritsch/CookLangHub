//! CookLangHub: a collaborative Cooklang Recipe platform backed by Forgejo.
//!
//! Forgejo and Git hold the authoritative Recipe and Cookbook state. This
//! application keeps operational state only, and every piece of it is
//! rebuildable.

pub mod auth;
pub mod bootstrap;
pub mod config;
pub mod create_recipe;
pub mod crypto;
pub mod db;
pub mod forgejo;
pub mod git;
pub mod health;
pub mod recipe;
pub mod render;
pub mod secret;
pub mod session;
pub mod telemetry;
pub mod theme;
pub mod upload;
pub mod web;
pub mod web_discussions;
pub mod web_recipes;

pub use config::Config;
