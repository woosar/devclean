use std::path::PathBuf;

use crate::data_structures::{Language, Root};

/// One visible row of the flattened explorer tree.
#[derive(Debug, Clone)]
pub struct TreeNode {
    pub path: PathBuf,
    pub name: String,
    pub depth: usize,
    pub expanded: bool,
    /// Known to have no (visible) subdirectories.
    pub leaf: bool,
    pub hidden: bool,
    /// Set for depth-0 nodes only.
    pub root: Option<Root>,
    /// Project manifests found directly in this directory.
    pub languages: Vec<Language>,
    /// This directory or an ancestor is a Python project.
    pub python_scope: bool,
    /// This directory is build output of a project of that language.
    pub relic: Option<Language>,
}
