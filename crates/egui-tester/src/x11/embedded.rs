use std::{
    thread,
    time::{Duration, Instant},
};

use x11rb::protocol::xproto::{ConnectionExt as _, Window as WindowId};

use crate::{Application, Error, Result};

use super::{Window, X11Controller, window, x11};

impl X11Controller {
    /// Evict an embedded window to the root and await its same-title redock.
    ///
    /// This exercises XEmbed-style tray recovery without admitting ambient
    /// desktop tools or authority into the testbed. The returned window may
    /// have a new X11 identity if the application reforged it.
    pub fn evict_and_wait_redocked(
        &self,
        app: &Application<'_>,
        window: &Window,
        timeout: Duration,
    ) -> Result<Window> {
        let root = self.root();
        let parent = self
            .connection
            .query_tree(window.id)
            .map_err(|err| x11("query embedded window parent", err))?
            .reply()
            .map_err(|err| x11("query embedded window parent", err))?
            .parent;
        if parent == root {
            return Err(Error::X11 {
                operation: "evict embedded window",
                detail: format!("window `{}` is already a root child", window.title),
            });
        }
        self.connection
            .reparent_window(window.id, root, 0, 0)
            .map_err(|err| x11("evict embedded window", err))?
            .check()
            .map_err(|err| x11("evict embedded window", err))?;
        self.flush("publish embedded window eviction")?;

        let deadline = Instant::now() + timeout;
        loop {
            app.ensure_running(format!("window `{}` to redock", window.title))?;
            if let Some(redocked) = self.find_named_child(parent, &window.title)? {
                return Ok(redocked);
            }
            if Instant::now() >= deadline {
                return Err(Error::Timeout {
                    waiting: format!("window `{}` to redock", window.title),
                    timeout,
                });
            }
            thread::sleep(Duration::from_millis(15));
        }
    }

    fn find_named_child(&self, parent: WindowId, title: &str) -> Result<Option<Window>> {
        let net_name = self.atom("_NET_WM_NAME")?;
        let utf8 = self.atom("UTF8_STRING")?;
        let tree = self
            .connection
            .query_tree(parent)
            .map_err(|err| x11("query embedding parent", err))?;
        let Some(tree) = window::reply("query embedding parent", tree.reply())? else {
            return Ok(None);
        };
        for child in tree.children {
            if window::title(&self.connection, child, net_name, utf8)?.as_deref() == Some(title)
                && window::viewable(&self.connection, child)?
            {
                return Ok(Some(Window {
                    id: child,
                    title: title.to_owned(),
                }));
            }
        }
        Ok(None)
    }
}
