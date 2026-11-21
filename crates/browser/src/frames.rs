// https://html.spec.whatwg.org/multipage/browsers.html#determining-the-creation-sandboxing-flags
// https://w3c.github.io/webappsec-csp/#sandbox

use std::collections::HashMap;

use renderer::{FrameId, FrameKind, SandboxFlags, SerializedOrigin};
use url::Url;

use crate::site::Site;

#[derive(Clone, Debug)]
struct FrameEntry {
    parent: FrameId,
    creation: SandboxFlags,
    active: SandboxFlags,
    origin: Option<url::Origin>,
    site: Option<Site>,
}

#[derive(Default)]
pub(crate) struct FrameTree {
    popup_sandbox: SandboxFlags,
    main_csp: SandboxFlags,
    main_origin: Option<url::Origin>,
    children: HashMap<FrameId, FrameEntry>,
}

impl FrameTree {
    pub(crate) fn set_main_origin(&mut self, origin: url::Origin) {
        self.main_origin = Some(origin);
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
        self.children.insert(
            frame,
            FrameEntry {
                parent,
                creation,
                active: creation,
                origin: None,
                site: None,
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

    pub(crate) fn authorize_url(&self, main: &Site, spec: &str) -> Option<Url> {
        main.authorize_url(spec).or_else(|| {
            self.children
                .values()
                .filter_map(|entry| entry.site.as_ref())
                .find_map(|site| site.authorize_url(spec))
        })
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
