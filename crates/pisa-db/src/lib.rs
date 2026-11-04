//! The metadata that workers keep in a database, shared with the search
//! service, which reads the snapshot catalog.

mod backend;
pub mod catalog;
pub mod db;
pub mod records;
mod schema;
