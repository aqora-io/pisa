//! The metadata that workers keep in a database, shared with the search
//! service, which reads the snapshot catalog, and the queue through which
//! servers hand jobs to workers.

mod backend;
pub mod catalog;
pub mod db;
pub mod description_flags;
pub mod environments;
pub mod queue;
pub mod records;
mod schema;
pub mod scope;
