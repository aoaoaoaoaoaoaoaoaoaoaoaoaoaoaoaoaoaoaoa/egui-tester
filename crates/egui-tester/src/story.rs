use std::{borrow::Cow, time::Duration};

use serde::de::DeserializeOwned;

use crate::{
    ActionReceipt, Anchor, Application, Button, Condition, Drag, Frame, Key, Modifiers,
    PerformanceBudget, Probe, ProbeFrame, Result, Stroke, Testbed, Timed, Wheel, WindowQuery,
    X11Session,
};

/// Stable identity of a visible gesture recipient.
///
/// Product contract enums should implement this trait. Strings remain lawful
/// as an escape hatch while a contract is incubating.
pub trait Target {
    fn wire_name(&self) -> Cow<'_, str>;
}

impl<T: AsRef<str> + ?Sized> Target for T {
    fn wire_name(&self) -> Cow<'_, str> {
        Cow::Borrowed(self.as_ref())
    }
}

/// Typed, native-input story context for one running application.
pub struct Story<'app, 'bed, S> {
    session: X11Session<'app, 'bed>,
    probe: Probe<S>,
    reaction_budget: PerformanceBudget,
    target_timeout: Duration,
    wait_timeout: Duration,
}

impl<'app, 'bed, S: DeserializeOwned + 'static> Story<'app, 'bed, S> {
    /// Bind a typed story to the product's standard witness and native window.
    pub fn bind(
        testbed: &'bed Testbed,
        app: &'app Application<'bed>,
        query: impl Into<WindowQuery>,
        reaction_budget: PerformanceBudget,
    ) -> Result<Self> {
        let session = testbed.x11_session(app, query, Duration::from_secs(30))?;
        session.focus()?;
        Ok(Self {
            session,
            probe: app.witness()?.typed(),
            reaction_budget,
            target_timeout: Duration::from_secs(8),
            wait_timeout: Duration::from_secs(10),
        })
    }

    #[must_use]
    pub const fn with_target_timeout(mut self, timeout: Duration) -> Self {
        self.target_timeout = timeout;
        self
    }

    #[must_use]
    pub const fn with_wait_timeout(mut self, timeout: Duration) -> Self {
        self.wait_timeout = timeout;
        self
    }

    #[must_use]
    pub const fn session(&self) -> &X11Session<'app, 'bed> {
        &self.session
    }

    #[must_use]
    pub const fn probe(&self) -> &Probe<S> {
        &self.probe
    }

    pub const fn probe_mut(&mut self) -> &mut Probe<S> {
        &mut self.probe
    }

    pub fn ready(&mut self, timeout: Duration) -> Result<ProbeFrame<S>> {
        self.probe
            .wait_presented(self.session.application(), timeout)
    }

    pub fn frame(&self) -> Result<ProbeFrame<S>> {
        self.probe.read()
    }

    pub fn wait(&mut self, condition: Condition<S>) -> Result<ProbeFrame<S>> {
        self.wait_within(self.wait_timeout, condition)
    }

    pub fn wait_within(
        &mut self,
        timeout: Duration,
        condition: Condition<S>,
    ) -> Result<ProbeFrame<S>> {
        let description = condition.description().to_owned();
        self.probe
            .wait_checked(self.session.application(), timeout, description, |frame| {
                condition.evaluate(&frame.state)
            })
    }

    pub fn wait_stable<T: PartialEq>(
        &mut self,
        timeout: Duration,
        quiet: Duration,
        description: impl Into<String>,
        project: impl FnMut(&ProbeFrame<S>) -> Option<T>,
    ) -> Result<ProbeFrame<S>> {
        self.probe.wait_stable(
            self.session.application(),
            timeout,
            quiet,
            description,
            project,
        )
    }

    pub fn anchor(&mut self, target: impl Target) -> Result<Anchor> {
        let name = target.wire_name();
        self.probe.wait_anchor(
            self.session.application(),
            name.as_ref(),
            self.target_timeout,
        )
    }

    pub fn click(&mut self, target: impl Target) -> Result<Reaction<'_, 'app, 'bed, S>> {
        let target = target.wire_name().into_owned();
        let anchor = self.anchor(target.as_str())?;
        let receipt = self
            .session
            .click(anchor.center().0, anchor.center().1, Button::Primary)?;
        Ok(self.reaction_named(receipt, format!("click `{target}`")))
    }

    pub fn modified_click(
        &mut self,
        target: impl Target,
        button: Button,
        modifiers: Modifiers,
    ) -> Result<Reaction<'_, 'app, 'bed, S>> {
        let target = target.wire_name().into_owned();
        let anchor = self.anchor(target.as_str())?;
        let (x, y) = anchor.center();
        let receipt = self.session.modified_click(x, y, button, modifiers)?;
        Ok(self.reaction_named(receipt, format!("{modifiers:?} click `{target}`")))
    }

    pub fn click_anchor(&mut self, anchor: &Anchor) -> Result<Reaction<'_, 'app, 'bed, S>> {
        let (x, y) = anchor.center();
        let receipt = self.session.click(x, y, Button::Primary)?;
        Ok(self.reaction_named(receipt, format!("click `{}`", anchor.name)))
    }

    pub fn click_at(
        &mut self,
        point: (i16, i16),
        button: Button,
    ) -> Result<Reaction<'_, 'app, 'bed, S>> {
        let receipt = self.session.click(point.0, point.1, button)?;
        Ok(self.reaction(receipt))
    }

    pub fn modified_click_at(
        &mut self,
        point: (i16, i16),
        button: Button,
        modifiers: Modifiers,
    ) -> Result<Reaction<'_, 'app, 'bed, S>> {
        let receipt = self
            .session
            .modified_click(point.0, point.1, button, modifiers)?;
        Ok(self.reaction(receipt))
    }

    pub fn drag(
        &mut self,
        target: impl Target,
        destination: (i16, i16),
        policy: Drag,
    ) -> Result<Reaction<'_, 'app, 'bed, S>> {
        let target = target.wire_name().into_owned();
        let origin = self.anchor(target.as_str())?.center();
        let receipt = self.session.drag(origin, destination, policy)?;
        Ok(self.reaction_named(receipt, format!("drag `{target}`")))
    }

    pub fn drag_from(
        &mut self,
        origin: (i16, i16),
        destination: (i16, i16),
        policy: Drag,
    ) -> Result<Reaction<'_, 'app, 'bed, S>> {
        let receipt = self.session.drag(origin, destination, policy)?;
        Ok(self.reaction(receipt))
    }

    pub fn stroke(
        &mut self,
        knots: &[(i16, i16)],
        policy: Stroke,
    ) -> Result<Reaction<'_, 'app, 'bed, S>> {
        let receipt = self.session.stroke(knots, policy)?;
        Ok(self.reaction(receipt))
    }

    pub fn wheel(
        &mut self,
        point: (i16, i16),
        ticks: i32,
        policy: Wheel,
    ) -> Result<Reaction<'_, 'app, 'bed, S>> {
        let receipt = self.session.wheel(point.0, point.1, ticks, policy)?;
        Ok(self.reaction(receipt))
    }

    pub fn key(&mut self, key: Key) -> Result<Reaction<'_, 'app, 'bed, S>> {
        let receipt = self.session.key(key)?;
        Ok(self.reaction(receipt))
    }

    pub fn chord(&mut self, modifiers: Modifiers, key: Key) -> Result<Reaction<'_, 'app, 'bed, S>> {
        let receipt = self.session.chord(modifiers, key)?;
        Ok(self.reaction(receipt))
    }

    pub fn type_text(&mut self, text: &str) -> Result<Reaction<'_, 'app, 'bed, S>> {
        let receipt = self.session.type_text(text)?;
        Ok(self.reaction(receipt))
    }

    /// Replace one text target through the same focus, selection, and typing
    /// sequence used by a person.
    pub fn replace_text(
        &mut self,
        target: impl Target,
        text: &str,
        focused: Condition<S>,
    ) -> Result<Reaction<'_, 'app, 'bed, S>> {
        let _focused = self.click(target)?.expect(focused)?;
        let _selected = self.session.chord(Modifiers::CTRL, Key::Character('a'))?;
        self.type_text(text)
    }

    pub fn reaction(&mut self, receipt: ActionReceipt) -> Reaction<'_, 'app, 'bed, S> {
        let description = receipt.action().to_owned();
        self.reaction_named(receipt, description)
    }

    pub fn capture(&self) -> Result<Frame> {
        self.session.capture()
    }

    fn reaction_named(
        &mut self,
        receipt: ActionReceipt,
        description: String,
    ) -> Reaction<'_, 'app, 'bed, S> {
        let budget = self.reaction_budget;
        Reaction {
            story: self,
            receipt,
            budget,
            description,
        }
    }
}

/// One injected gesture awaiting causally fresh product evidence.
pub struct Reaction<'story, 'app, 'bed, S> {
    story: &'story mut Story<'app, 'bed, S>,
    receipt: ActionReceipt,
    budget: PerformanceBudget,
    description: String,
}

impl<S: DeserializeOwned + 'static> Reaction<'_, '_, '_, S> {
    #[must_use]
    pub fn within(&mut self, budget: PerformanceBudget) -> &mut Self {
        self.budget = budget;
        self
    }

    pub const fn receipt(&self) -> &ActionReceipt {
        &self.receipt
    }

    #[must_use]
    pub const fn session(&self) -> &X11Session<'_, '_> {
        &self.story.session
    }

    pub fn expect(&mut self, condition: Condition<S>) -> Result<Timed<ProbeFrame<S>>> {
        let description = format!("{} to make {}", self.description, condition.description());
        self.story.probe.wait_budgeted_checked(
            self.story.session.application(),
            &self.receipt,
            self.budget,
            description,
            |frame| condition.evaluate(&frame.state),
        )
    }

    pub fn presented(&mut self) -> Result<Timed<ProbeFrame<S>>> {
        self.expect(Condition::new("a fresh presented frame", |_| true))
    }
}

/// Turn an external product fact into a first-class acceptance verdict.
pub fn demand(condition: bool, detail: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(crate::Error::Verdict {
            detail: detail.into(),
        })
    }
}
