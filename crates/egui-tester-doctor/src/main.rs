use std::time::Duration;

use egui_tester::{AppCommand, Error, Result, Testbed};

fn main() -> Result<()> {
    let testbed = Testbed::raise()?;
    let app = testbed.launch(AppCommand::new("/usr/bin/true").runtime(Duration::from_secs(5)))?;
    let exit = app.wait(Duration::from_secs(5))?;
    app.terminate()?;
    if !exit.success() {
        return Err(Error::Command {
            command: "/usr/bin/true".to_owned(),
            status: format!("code {}, result {}", exit.code, exit.result),
            stderr: exit.stderr,
        });
    }
    println!(
        "egui-tester host is ready; display, containment, service, and software graphics passed under {}",
        testbed.id()
    );
    Ok(())
}
