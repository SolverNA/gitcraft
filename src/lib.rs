pub mod artifacts;
pub mod config;
pub mod deploy;
pub mod gitrepo;
pub mod rcon;
pub mod transfer;
pub mod updater;
pub mod web;

pub mod recovery;

pub mod adopt;

pub mod policy;

pub mod export;

pub mod repository;

#[cfg(test)]
mod e2e;

pub mod secrets;

pub mod ops;

pub mod ops_ui;

pub mod agent_loop;

pub mod agent_service;

#[cfg(test)]
mod activation_acceptance;

pub mod minecraft_autostart;
