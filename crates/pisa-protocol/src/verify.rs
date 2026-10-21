//! Outcome of the verification policy that workers apply to checked code.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verification {
    /// True when the code elaborated without errors and passed every policy check.
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub declarations: Vec<Declaration>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Declaration {
    pub name: String,
    pub kind: String,
    pub axioms: Vec<String>,
    #[serde(rename = "typeHash")]
    pub type_hash: String,
}
