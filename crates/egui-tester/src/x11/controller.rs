use x11rb::rust_connection::RustConnection;

/// Authenticated control connection to the testbed's private X server.
pub struct X11Controller {
    pub(super) connection: RustConnection,
    pub(super) screen: usize,
}

impl std::fmt::Debug for X11Controller {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("X11Controller")
            .field("screen", &self.screen)
            .finish_non_exhaustive()
    }
}
