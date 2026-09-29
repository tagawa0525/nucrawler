pub mod check;
pub mod cli;
pub mod config;
pub mod db;
pub mod errors;
pub mod eval;
pub mod extract;
pub mod glossary;
pub mod http;
pub mod jst;
pub mod llm;
pub mod mcp;
pub mod pipeline;
pub mod profile;
pub mod prompt;
pub mod quota;
pub mod recommend;
pub mod robots;
pub mod search;
pub mod source;
pub mod status;
pub mod suggest;
pub mod text;
pub mod topics;
pub mod web;

#[cfg(test)]
mod testutil;
