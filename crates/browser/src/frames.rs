// https://html.spec.whatwg.org/multipage/browsers.html#determining-the-creation-sandboxing-flags
// https://w3c.github.io/webappsec-csp/#sandbox

use std::collections::HashMap;

use renderer::{ContextName, FrameId, FrameKind, SandboxFlags, SerializedOrigin};
use url::Url;

use crate::site::Site;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameInfo {
    pub id: FrameId,
    pub parent: Option<FrameId>,
    pub url: String,
    pub name: Option<ContextName>,
}

#[derive(Clone, Debug)]
struct FrameEntry {
    parent: FrameId,
    creation: SandboxFlags,
    active: SandboxFlags,
    origin: Option<url::Origin>,
    site: Option<Site>,
    url: String,
    name: Option<ContextName>,
    order: u64,
}

#[derive(Default)]
pub(crate) struct FrameTree {
    popup_sandbox: SandboxFlags,
    main_csp: SandboxFlags,
    main_origin: Option<url::Origin>,
    main_url: String,
    next_order: u64,
    children: HashMap<FrameId, FrameEntry>,
}

impl FrameTree {
    pub(crate) fn set_main_origin(&mut self, origin: url::Origin) {
        self.main_origin = Some(origin);
    }

    pub(crate) fn set_main_url(&mut self, url: String) {
        self.main_url = url;
    }

    pub(crate) fn set_child_url(&mut self, frame: FrameId, url: String) {
        if let Some(entry) = self.children.get_mut(&frame) {
            entry.url = url;
        }
    }

    #[must_use]
    pub(crate) fn parent_of(&self, frame: FrameId) -> Option<FrameId> {
        self.children.get(&frame).map(|entry| entry.parent)
    }

    #[must_use]
    pub(crate) fn frame_url(&self, frame: FrameId) -> Option<String> {
        if frame == FrameId::MAIN {
            return Some(if self.main_url.is_empty() {
                "about:blank".to_owned()
            } else {
                self.main_url.clone()
            });
        }
        self.children.get(&frame).map(|entry| entry.url.clone())
    }

    #[must_use]
    pub(crate) fn snapshot(&self, main_name: Option<ContextName>) -> Vec<FrameInfo> {
        let mut out = Vec::with_capacity(self.children.len() + 1);
        out.push(FrameInfo {
            id: FrameId::MAIN,
            parent: None,
            url: if self.main_url.is_empty() {
                "about:blank".to_owned()
            } else {
                self.main_url.clone()
            },
            name: main_name,
        });
        let mut ordered: Vec<(FrameId, &FrameEntry)> = self
            .children
            .iter()
            .map(|(&id, entry)| (id, entry))
            .collect();
        ordered.sort_by_key(|(_, entry)| entry.order);
        for (id, entry) in ordered {
            out.push(FrameInfo {
                id,
                parent: Some(entry.parent),
                url: entry.url.clone(),
                name: entry.name.clone(),
            });
        }
        out
    }

    pub(crate) fn ancestor_origins(&self, frame: FrameId) -> Vec<url::Origin> {
        let mut ancestors = Vec::new();
        let mut current = self.children.get(&frame).map(|entry| entry.parent);
        let mut steps = 0usize;
        while let Some(parent) = current {
            if steps > self.children.len() {
                break;
            }
            steps += 1;
            if parent == FrameId::MAIN {
                if let Some(origin) = &self.main_origin {
                    ancestors.push(origin.clone());
                }
                break;
            }
            let Some(entry) = self.children.get(&parent) else {
                break;
            };
            if let Some(origin) = &entry.origin {
                ancestors.push(origin.clone());
            }
            current = Some(entry.parent);
        }
        ancestors
    }
    // https://html.spec.whatwg.org/multipage/browsers.html#popup-sandboxing-flag-set
    pub(crate) fn set_popup_sandbox(&mut self, sandbox: SandboxFlags) {
        self.popup_sandbox = sandbox;
    }

    #[cfg(test)]
    pub(crate) fn popup_sandbox(&self) -> SandboxFlags {
        self.popup_sandbox
    }

    pub(crate) fn main_csp(&self) -> SandboxFlags {
        self.main_csp
    }

    pub(crate) fn reset_main_csp(&mut self, csp: SandboxFlags) {
        self.main_csp = csp;
    }

    pub(crate) fn register(
        &mut self,
        frame: FrameId,
        parent: FrameId,
        kind: FrameKind,
        reported: SandboxFlags,
    ) -> bool {
        if frame == FrameId::MAIN || self.children.contains_key(&frame) {
            return false;
        }
        let parent_active = match self.children.get(&parent) {
            Some(parent_record) => parent_record.active,
            None if parent == FrameId::MAIN => self.popup_sandbox.union(self.main_csp),
            None => return false,
        };
        let creation = match kind {
            FrameKind::Child => parent_active.union(reported),
            FrameKind::Auxiliary => parent_active.auxiliary_flags(),
        };
        let order = self.next_order;
        self.next_order = self.next_order.saturating_add(1);
        self.children.insert(
            frame,
            FrameEntry {
                parent,
                creation,
                active: creation,
                origin: None,
                site: None,
                url: "about:blank".to_owned(),
                name: None,
                order,
            },
        );
        true
    }

    pub(crate) fn sandbox(&self, frame: FrameId) -> Option<SandboxFlags> {
        if frame == FrameId::MAIN {
            return Some(self.popup_sandbox.union(self.main_csp));
        }
        self.children.get(&frame).map(|entry| entry.active)
    }

    pub(crate) fn commit_child(&mut self, frame: FrameId, url: &Url, csp: SandboxFlags) {
        if frame == FrameId::MAIN {
            return;
        }
        if let Some(entry) = self.children.get_mut(&frame) {
            entry.active = entry.creation.union(csp);
            entry.url = url.to_string();
            if entry.active.origin_is_opaque() {
                entry.origin = Some(url::Origin::new_opaque());
                entry.site = None;
            } else {
                entry.origin = Some(url.origin());
                entry.site = Site::for_url(url);
            }
        }
    }

    pub(crate) fn remove(&mut self, frame: FrameId) -> bool {
        if frame == FrameId::MAIN {
            return false;
        }
        let mut stack = vec![frame];
        while let Some(target) = stack.pop() {
            self.children.remove(&target);
            let children: Vec<FrameId> = self
                .children
                .iter()
                .filter_map(|(id, entry)| (entry.parent == target).then_some(*id))
                .collect();
            stack.extend(children);
        }
        true
    }

    pub(crate) fn clear(&mut self) {
        self.children.clear();
    }

    pub(crate) fn authorize_origin(
        &self,
        main: &Site,
        claim: &SerializedOrigin,
    ) -> Option<url::Origin> {
        main.authorize_origin(claim).or_else(|| {
            self.children
                .values()
                .filter_map(|entry| entry.site.as_ref())
                .find_map(|site| site.authorize_origin(claim))
        })
    }

    pub(crate) fn allows_origin(&self, main: &Site, origin: &url::Origin) -> bool {
        main.allows_origin(origin)
            || self
                .children
                .values()
                .filter_map(|entry| entry.site.as_ref())
                .any(|site| site.allows_origin(origin))
    }

    pub(crate) fn allows_url(&self, main: &Site, url: &Url) -> bool {
        main.allows_url(url)
            || self
                .children
                .values()
                .filter_map(|entry| entry.site.as_ref())
                .any(|site| site.allows_url(url))
    }

    pub(crate) fn allows_message_origin(&self, main: &Site, claim: &SerializedOrigin) -> bool {
        main.allows_message_origin(claim)
            || self
                .children
                .values()
                .filter_map(|entry| entry.site.as_ref())
                .any(|site| site.allows_message_origin(claim))
    }
}
