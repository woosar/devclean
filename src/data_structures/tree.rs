use std::path::{Path, PathBuf};

use ratatui::widgets::ListState;

use crate::{
    data_structures::{Root, RootKind, TreeNode},
    helpers,
};

/// Lazily loaded directory explorer, stored as a flattened list of visible rows.
pub struct Tree {
    pub nodes: Vec<TreeNode>,
    pub state: ListState,
    pub show_hidden: bool,
    roots: Vec<Root>,
}

impl Tree {
    pub fn new(roots: Vec<Root>) -> Self {
        let mut tree = Self { nodes: Vec::new(), state: ListState::default(), show_hidden: false, roots };
        tree.reset_nodes();
        tree
    }

    fn reset_nodes(&mut self) {
        self.nodes = self.roots.iter().cloned().map(helpers::root_node).collect();
        self.state.select((!self.nodes.is_empty()).then_some(0));
    }

    pub fn selected_index(&self) -> Option<usize> {
        self.state.selected().filter(|&i| i < self.nodes.len())
    }

    pub fn selected(&self) -> Option<&TreeNode> {
        self.selected_index().map(|i| &self.nodes[i])
    }

    /// The root whose path is the longest prefix of `path`: the volume or mount it lives on.
    pub fn volume_for(&self, path: &Path) -> Option<&Root> {
        self.roots
            .iter()
            .filter(|r| r.kind != RootKind::Home && helpers::path_starts_with(path, &r.path))
            .max_by_key(|r| r.path.components().count())
            .or_else(|| self.roots.iter().find(|r| helpers::path_starts_with(path, &r.path)))
    }

    pub fn root_paths(&self) -> Vec<PathBuf> {
        self.roots.iter().map(|r| r.path.clone()).collect()
    }

    pub fn move_by(&mut self, delta: isize) {
        if self.nodes.is_empty() {
            return;
        }
        let current = self.selected_index().unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, self.nodes.len() as isize - 1);
        self.state.select(Some(next as usize));
    }

    pub fn select_first(&mut self) {
        self.move_by(isize::MIN / 2);
    }

    pub fn select_last(&mut self) {
        self.move_by(isize::MAX / 2);
    }

    fn expand(&mut self, index: usize) {
        let node = &self.nodes[index];
        if node.expanded || node.leaf {
            return;
        }
        let children = helpers::load_children(node, self.show_hidden);
        let node = &mut self.nodes[index];
        if children.is_empty() {
            node.leaf = true;
            return;
        }
        node.expanded = true;
        self.nodes.splice(index + 1..index + 1, children);
    }

    fn collapse(&mut self, index: usize) {
        let depth = self.nodes[index].depth;
        let end = self.nodes[index + 1..].iter().position(|n| n.depth <= depth).map_or(self.nodes.len(), |p| index + 1 + p);
        self.nodes.drain(index + 1..end);
        self.nodes[index].expanded = false;
    }

    fn parent_of(&self, index: usize) -> Option<usize> {
        let depth = self.nodes[index].depth;
        (0..index).rev().find(|&i| self.nodes[i].depth < depth)
    }

    /// Expand, or step into the first child when already expanded.
    pub fn right(&mut self) {
        let Some(index) = self.selected_index() else { return };
        if self.nodes[index].expanded {
            self.move_by(1);
        } else {
            self.expand(index);
        }
    }

    /// Collapse, or jump to the parent when already collapsed.
    pub fn left(&mut self) {
        let Some(index) = self.selected_index() else { return };
        if self.nodes[index].expanded {
            self.collapse(index);
        } else {
            self.select_parent();
        }
    }

    pub fn select_parent(&mut self) {
        if let Some(parent) = self.selected_index().and_then(|i| self.parent_of(i)) {
            self.state.select(Some(parent));
        }
    }

    /// Expand the tree down to `target` and select it.
    pub fn reveal(&mut self, target: &Path) -> bool {
        let best = self
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.depth == 0 && helpers::path_starts_with(target, &n.path))
            .max_by_key(|(_, n)| n.path.components().count())
            .map(|(i, _)| i);
        let Some(mut index) = best else { return false };

        let skip = self.nodes[index].path.components().count();
        for component in target.components().skip(skip) {
            let name = component.as_os_str();
            self.expand(index);
            let depth = self.nodes[index].depth + 1;
            let found = self.nodes[index + 1..]
                .iter()
                .take_while(|n| n.depth >= depth)
                .position(|n| n.depth == depth && n.path.file_name().is_some_and(|f| helpers::names_equal(f, name)));
            match found {
                Some(offset) => index += 1 + offset,
                None if !self.show_hidden && helpers::is_hidden(&name.to_string_lossy()) => {
                    self.show_hidden = true;
                    self.rebuild();
                    return self.reveal(target);
                }
                None => break,
            }
        }
        self.state.select(Some(index));
        true
    }

    /// Add a root for a location no existing root covers (e.g. a UNC share).
    pub fn pin(&mut self, root: Root) {
        self.roots.push(root);
        self.rebuild();
    }

    /// Reload everything from disk, keeping expanded folders and the selection.
    pub fn rebuild(&mut self) {
        let expanded: Vec<PathBuf> = self.nodes.iter().filter(|n| n.expanded).map(|n| n.path.clone()).collect();
        let selected = self.selected().map(|n| n.path.clone());
        self.reset_nodes();
        for path in &expanded {
            if let Some(index) = self.nodes.iter().position(|n| &n.path == path) {
                self.expand(index);
            }
        }
        let Some(mut wanted) = selected else { return };
        loop {
            if let Some(index) = self.nodes.iter().position(|n| n.path == wanted) {
                self.state.select(Some(index));
                return;
            }
            if !wanted.pop() {
                return;
            }
        }
    }

    pub fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        self.rebuild();
    }
}
