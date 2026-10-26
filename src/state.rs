//! Filtered/sorted/paged view state: items, active tab, filters, sort, cart and
//! taxonomy (mirrors `asset-browser/src/lib/state.js`, but the cart is an ordered
//! `Vec<String>` of codes and the taxonomy maps are pre-computed).
//!
//! Invariant: `page` stays inside `0 .. page_count()` — every filter, tab or sort
//! change ends by calling `clamp_page()`, so the page can never run past the last.

use std::collections::{BTreeMap, BTreeSet};

use crate::model::{format_int, Item, Kind};

/// Sort order. Triangle sorts are model-only.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortKey {
    Code,
    Slug,
    TriAsc,
    TriDesc,
}

impl SortKey {
    pub fn from_index(i: i32) -> SortKey {
        match i {
            1 => SortKey::Slug,
            2 => SortKey::TriAsc,
            3 => SortKey::TriDesc,
            _ => SortKey::Code,
        }
    }

    /// Index into `sort_options()`.
    pub fn index(self) -> i32 {
        match self {
            SortKey::Code => 0,
            SortKey::Slug => 1,
            SortKey::TriAsc => 2,
            SortKey::TriDesc => 3,
        }
    }
}

/// L1 → L2 vocabulary per kind, keyed by `Kind::as_str()`.
#[derive(Clone, Default)]
pub struct Taxonomy {
    pub by_kind: BTreeMap<String, BTreeMap<String, Vec<String>>>,
}

impl Taxonomy {
    /// Sorted L1 options for one kind (used by the detail editor).
    pub fn l1_for(&self, k: Kind) -> Vec<String> {
        let mut v: Vec<String> = self
            .by_kind
            .get(k.as_str())
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        v.sort_by(|a, b| en_cmp(a, b));
        v.dedup();
        v
    }

    /// Sorted L2 options for one kind + L1 (used by the detail editor).
    pub fn l2_for(&self, k: Kind, l1: &str) -> Vec<String> {
        let mut v = self
            .by_kind
            .get(k.as_str())
            .and_then(|m| m.get(l1))
            .cloned()
            .unwrap_or_default();
        v.sort_by(|a, b| en_cmp(a, b));
        v.dedup();
        v
    }
}

pub struct AppState {
    pub items: Vec<Item>,
    pub tab: Kind,
    pub q: String,
    pub l1: String,
    pub l2: String,
    pub tag: String,
    pub style_textured_only: bool,
    pub sort: SortKey,
    /// Current 0-based page of the filtered list.
    pub page: usize,
    /// Cards per page (always one of `page_size_options()`).
    pub page_size: usize,
    /// Cards per row (always one of `column_options()`).
    pub columns: usize,
    /// Codes, in insertion order.
    pub cart: Vec<String>,
    pub taxonomy: Taxonomy,
}

impl AppState {
    pub fn new(items: Vec<Item>, cart: Vec<String>, taxonomy: Taxonomy) -> Self {
        let mut s = AppState {
            items,
            tab: Kind::Model,
            q: String::new(),
            l1: String::new(),
            l2: String::new(),
            tag: String::new(),
            style_textured_only: false,
            sort: SortKey::Code,
            page: 0,
            page_size: 12,
            columns: 6,
            cart,
            taxonomy,
        };
        let valid: std::collections::HashSet<String> =
            s.items.iter().map(|it| it.code.clone()).collect();
        s.cart.retain(|c| valid.contains(c));
        s
    }

    pub fn kind_count(&self, k: Kind) -> i64 {
        self.items.iter().filter(|it| it.kind() == k).count() as i64
    }

    pub fn l1_options(&self) -> Vec<String> {
        let mut v = self.taxonomy.l1_for(self.tab);
        if v.is_empty() {
            let mut set: BTreeSet<String> = BTreeSet::new();
            for it in &self.items {
                if it.kind() == self.tab && !it.l1.is_empty() {
                    set.insert(it.l1.clone());
                }
            }
            v = set.into_iter().collect();
            v.sort_by(|a, b| en_cmp(a, b));
        }
        v
    }

    pub fn l2_options(&self) -> Vec<String> {
        if self.l1.is_empty() {
            return Vec::new();
        }
        let mut v = self.taxonomy.l2_for(self.tab, &self.l1);
        if v.is_empty() {
            let mut set: BTreeSet<String> = BTreeSet::new();
            for it in &self.items {
                if it.kind() == self.tab && it.l1 == self.l1 && !it.l2.is_empty() {
                    set.insert(it.l2.clone());
                }
            }
            v = set.into_iter().collect();
            v.sort_by(|a, b| en_cmp(a, b));
        }
        v
    }

    /// Indices into `self.items` that pass the current filters, sorted.
    pub fn filtered(&self) -> Vec<usize> {
        let q = self.q.trim().to_lowercase();
        let mut out: Vec<usize> = Vec::new();
        for (i, it) in self.items.iter().enumerate() {
            if it.kind() != self.tab {
                continue;
            }
            if !q.is_empty() && !it.hay().contains(&q) {
                continue;
            }
            if !self.l1.is_empty() && it.l1 != self.l1 {
                continue;
            }
            if !self.l2.is_empty() && it.l2 != self.l2 {
                continue;
            }
            if !self.tag.is_empty() && !it.has_tag(&self.tag) {
                continue;
            }
            if self.style_textured_only && it.untextured {
                continue;
            }
            out.push(i);
        }
        match self.sort {
            SortKey::Slug => out.sort_by(|&a, &b| en_cmp(&self.items[a].slug, &self.items[b].slug)),
            SortKey::TriAsc => out.sort_by_key(|&a| self.items[a].tri_count),
            SortKey::TriDesc => {
                out.sort_by(|&a, &b| self.items[b].tri_count.cmp(&self.items[a].tri_count))
            }
            SortKey::Code => out.sort_by(|&a, &b| en_cmp(&self.items[a].code, &self.items[b].code)),
        }
        out
    }

    /// Number of items currently passing the filters.
    pub fn filtered_len(&self) -> usize {
        self.filtered().len()
    }

    /// Total pages, never zero (an empty result still reports one page).
    pub fn page_count(&self) -> usize {
        self.filtered_len().div_ceil(self.page_size).max(1)
    }

    /// Keep `page` inside `0 .. page_count()`.
    pub fn clamp_page(&mut self) {
        let last = self.page_count() - 1;
        if self.page > last {
            self.page = last;
        }
    }

    /// Change the page size (must be one of `page_size_options()`) while keeping
    /// the first currently-visible item on screen.
    pub fn set_page_size(&mut self, size: usize) {
        if !page_size_options().iter().any(|&s| s as usize == size) {
            return;
        }
        let first = self.page * self.page_size;
        self.page_size = size;
        self.page = first / size;
        self.clamp_page();
    }

    /// Change how many cards share a row (must be one of `column_options()`).
    /// The page itself does not change; the grid only re-chunks it.
    pub fn set_columns(&mut self, columns: usize) {
        if column_options().iter().any(|&c| c as usize == columns) {
            self.columns = columns;
        }
    }

    /// Global index of the first item on the current page.
    pub fn first_item_on_page(&self) -> usize {
        self.page * self.page_size
    }

    pub fn in_cart(&self, code: &str) -> bool {
        self.cart.iter().any(|c| c == code)
    }

    /// Returns `true` when the code is in the cart afterwards.
    pub fn toggle_cart(&mut self, code: &str) -> bool {
        if let Some(pos) = self.cart.iter().position(|c| c == code) {
            self.cart.remove(pos);
            false
        } else {
            self.cart.push(code.to_string());
            true
        }
    }

    /// Adds every currently filtered item; returns how many were newly added.
    pub fn add_all_filtered(&mut self) -> usize {
        let idxs = self.filtered();
        let mut added = 0usize;
        for i in idxs {
            let code = self.items[i].code.clone();
            if code.is_empty() || self.cart.iter().any(|c| c == &code) {
                continue;
            }
            self.cart.push(code);
            added += 1;
        }
        added
    }

    pub fn remove_cart(&mut self, code: &str) -> bool {
        if let Some(pos) = self.cart.iter().position(|c| c == code) {
            self.cart.remove(pos);
            true
        } else {
            false
        }
    }

    pub fn clear_cart(&mut self) {
        self.cart.clear();
    }

    pub fn cart_indices(&self) -> Vec<usize> {
        self.cart.iter().filter_map(|c| self.index_of(c)).collect()
    }

    pub fn index_of(&self, code: &str) -> Option<usize> {
        self.items.iter().position(|it| it.code == code)
    }

    /// Switch tab: resets L1/L2 and drops filters the new tab cannot satisfy.
    pub fn set_tab(&mut self, k: Kind) {
        self.tab = k;
        self.l1.clear();
        self.l2.clear();
        self.ensure_integrity();
        self.clamp_page();
    }

    pub fn reset_filters(&mut self) {
        self.q.clear();
        self.l1.clear();
        self.l2.clear();
        self.tag.clear();
        self.style_textured_only = false;
        self.page = 0;
        self.clamp_page();
    }

    pub fn filters_active(&self) -> bool {
        !self.q.is_empty()
            || !self.l1.is_empty()
            || !self.l2.is_empty()
            || !self.tag.is_empty()
            || self.style_textured_only
    }

    /// e.g. `"1,913 ASSETS"` or `"1,913 ASSETS · 42 matching"`.
    pub fn count_label(&self) -> String {
        let total = self.kind_count(self.tab);
        let shown = self.filtered().len() as i64;
        if shown == total {
            format!("{} {}", format_int(total), self.tab.noun())
        } else {
            format!(
                "{} {} · {} matching",
                format_int(total),
                self.tab.noun(),
                format_int(shown)
            )
        }
    }

    /// Set the sort from a `sort_options()` index, downgrading triangle sorts on
    /// tabs that have no triangle data.
    pub fn set_sort_index(&mut self, i: i32) {
        let next = SortKey::from_index(i);
        self.sort = match next {
            SortKey::TriAsc | SortKey::TriDesc
                if self.tab == Kind::Material || self.tab == Kind::Rig =>
            {
                SortKey::Code
            }
            other => other,
        };
        self.clamp_page();
    }

    /// Drop L1/L2 the active tab cannot satisfy and normalise the sort key.
    pub fn ensure_integrity(&mut self) {
        if !self.l1.is_empty() && !self.l1_options().iter().any(|x| x == &self.l1) {
            self.l1.clear();
            self.l2.clear();
        }
        if !self.l2.is_empty() && !self.l2_options().iter().any(|x| x == &self.l2) {
            self.l2.clear();
        }
        if (self.tab == Kind::Material || self.tab == Kind::Rig)
            && (self.sort == SortKey::TriAsc || self.sort == SortKey::TriDesc)
        {
            self.sort = SortKey::Code;
        }
    }
}

/// Build the taxonomy from `taxonomy.json` when a kind has a non-empty entry,
/// otherwise derive it from the items. Rigs are never in `taxonomy.json`.
pub fn build_taxonomy(items: &[Item], taxonomy_json: Option<&str>) -> Taxonomy {
    let parsed: Option<serde_json::Value> =
        taxonomy_json.and_then(|t| serde_json::from_str(t).ok());

    let mut by_kind: BTreeMap<String, BTreeMap<String, Vec<String>>> = BTreeMap::new();
    for k in [Kind::Model, Kind::Material, Kind::Rig] {
        let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut from_json = false;

        if k != Kind::Rig {
            if let Some(obj) = parsed
                .as_ref()
                .and_then(|v| v.get(k.as_str()))
                .and_then(|x| x.as_object())
            {
                if !obj.is_empty() {
                    for (l1, subs) in obj {
                        let mut list: Vec<String> = subs
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(|s| s.as_str().map(|x| x.to_string()))
                                    .collect()
                            })
                            .unwrap_or_default();
                        sort_unique(&mut list);
                        map.insert(l1.clone(), list);
                    }
                    from_json = true;
                }
            }
        }

        if !from_json {
            for it in items {
                if it.kind() != k || it.l1.is_empty() {
                    continue;
                }
                let entry = map.entry(it.l1.clone()).or_default();
                if !it.l2.is_empty() && !entry.contains(&it.l2) {
                    entry.push(it.l2.clone());
                }
            }
            for list in map.values_mut() {
                sort_unique(list);
            }
        }

        by_kind.insert(k.as_str().to_string(), map);
    }

    Taxonomy { by_kind }
}

pub fn sort_options() -> Vec<String> {
    vec![
        "Code".to_string(),
        "Slug".to_string(),
        "Triangles ↑".to_string(),
        "Triangles ↓".to_string(),
    ]
}

pub fn style_options() -> Vec<String> {
    vec!["All".to_string(), "Textured only".to_string()]
}

pub fn page_size_options() -> Vec<i32> {
    vec![12, 24, 48, 96]
}

/// Cards per row. Fewer columns mean bigger previews.
pub fn column_options() -> Vec<i32> {
    vec![3, 4, 5, 6, 8]
}

fn sort_unique(v: &mut Vec<String>) {
    v.sort_by(|a, b| en_cmp(a, b));
    v.dedup();
}

/// `en-US`-style numeric-aware, case-insensitive comparison (mirrors
/// `Intl.Collator('en', { numeric: true, sensitivity: 'base' })` closely enough
/// for the library's category/slug/code ordering).
fn en_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let ab = a.as_bytes();
    let bb = b.as_bytes();
    let (mut i, mut j) = (0usize, 0usize);
    while i < ab.len() && j < bb.len() {
        let ca = ab[i];
        let cb = bb[j];
        if ca.is_ascii_digit() && cb.is_ascii_digit() {
            let si = i;
            while i < ab.len() && ab[i].is_ascii_digit() {
                i += 1;
            }
            let sj = j;
            while j < bb.len() && bb[j].is_ascii_digit() {
                j += 1;
            }
            let na: u128 = a[si..i].parse().unwrap_or(0);
            let nb: u128 = b[sj..j].parse().unwrap_or(0);
            match na.cmp(&nb) {
                Ordering::Equal => {}
                other => return other,
            }
        } else {
            match ca.to_ascii_lowercase().cmp(&cb.to_ascii_lowercase()) {
                Ordering::Equal => {}
                other => return other,
            }
            i += 1;
            j += 1;
        }
    }
    (ab.len() - i).cmp(&(bb.len() - j))
}
