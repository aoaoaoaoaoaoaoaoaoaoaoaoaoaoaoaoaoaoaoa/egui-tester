use std::{fmt::Display, thread, time::Duration};

use serde::{Serialize, de::DeserializeOwned};

use crate::{
    ActionReceipt, Anchor, Application, Button, Condition, Drag, Frame, Key, Modifiers, Motion,
    Probe, ProbeFrame, ReactionBudget, Result, Stroke, Testbed, Timed, Wheel, WindowQuery,
    X11Session,
};

/// Authored editorial instruction carried by a live story stream.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(tag = "cue", rename_all = "snake_case")]
pub enum StoryCue<'a> {
    Chapter { title: &'a str },
    Hold { duration: Duration },
}

/// Immutable execution fact emitted while a story drives the product.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(tag = "fact", rename_all = "snake_case")]
pub enum StoryFact<'a> {
    TargetResolved {
        target: &'a str,
        anchor: &'a Anchor,
    },
    ActionDispatched {
        action: &'a str,
        target: Option<&'a str>,
        pointer: Option<[i16; 2]>,
        gesture_started_ns: u64,
        triggered_ns: u64,
        completed_ns: u64,
    },
    ObservationMatched {
        description: &'a str,
        schema: u32,
        frame: u64,
        begun_ns: u64,
        observed_ns: u64,
        surface_presented_ns: u64,
        surface_sequence: u64,
    },
}

/// One item in the causal stream produced by an executing story.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(tag = "stream", content = "event", rename_all = "snake_case")]
pub enum StoryEvent<'a> {
    Cue(StoryCue<'a>),
    Fact(StoryFact<'a>),
}

trait CaptureSurface {
    fn capture(&self) -> Result<Frame>;
}

impl CaptureSurface for X11Session<'_, '_> {
    fn capture(&self) -> Result<Frame> {
        X11Session::capture(self)
    }
}

/// Capture-only view of the private product surface offered to observers.
#[derive(Clone, Copy)]
pub struct StorySurface<'a> {
    surface: &'a dyn CaptureSurface,
}

impl StorySurface<'_> {
    pub fn capture(self) -> Result<Frame> {
        self.surface.capture()
    }
}

/// Synchronous consumer of one executing story's typed event stream.
pub trait StoryObserver {
    fn observe(&mut self, event: StoryEvent<'_>, surface: StorySurface<'_>) -> Result<()>;

    fn finish(&mut self, _surface: StorySurface<'_>) -> Result<()> {
        Ok(())
    }

    /// Whether this observer is admissible while production latency is judged.
    fn permits_performance_verdicts(&self) -> bool {
        false
    }
}

/// Inert default observer used by ordinary acceptance stories.
#[derive(Clone, Copy, Debug, Default)]
pub struct Silent;

impl StoryObserver for Silent {
    fn observe(&mut self, _event: StoryEvent<'_>, _surface: StorySurface<'_>) -> Result<()> {
        Ok(())
    }

    fn permits_performance_verdicts(&self) -> bool {
        true
    }
}

/// Typed, native-input story context for one running application.
pub struct Story<'app, 'bed, S, O = Silent> {
    session: X11Session<'app, 'bed>,
    probe: Probe<S>,
    observer: O,
    reaction_budget: ReactionBudget,
    target_timeout: Duration,
    wait_timeout: Duration,
}

impl<'app, 'bed, S: DeserializeOwned + 'static> Story<'app, 'bed, S, Silent> {
    /// Bind a typed story to the product's standard witness and native window.
    pub fn bind(
        testbed: &'bed Testbed,
        app: &'app Application<'bed>,
        query: impl Into<WindowQuery>,
        reaction_budget: ReactionBudget,
    ) -> Result<Self> {
        let session = testbed.x11_session(app, query, Duration::from_secs(30))?;
        session.focus()?;
        Ok(Self {
            session,
            probe: app.witness()?.typed(),
            observer: Silent,
            reaction_budget,
            target_timeout: Duration::from_secs(8),
            wait_timeout: Duration::from_secs(10),
        })
    }

    /// Attach one optional consumer before executing the story.
    pub fn with_observer<O: StoryObserver>(self, observer: O) -> Result<Story<'app, 'bed, S, O>> {
        if self.reaction_budget.production().is_some() && !observer.permits_performance_verdicts() {
            return Err(crate::Error::Unsupported {
                capability: "story observation during performance adjudication",
                detail: "recording or tracing would contaminate the production latency verdict"
                    .to_owned(),
            });
        }
        Ok(Story {
            session: self.session,
            probe: self.probe,
            observer,
            reaction_budget: self.reaction_budget,
            target_timeout: self.target_timeout,
            wait_timeout: self.wait_timeout,
        })
    }
}

impl<'app, 'bed, S: DeserializeOwned + 'static, O: StoryObserver> Story<'app, 'bed, S, O> {
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
        let result = self
            .probe
            .wait_surface_presented(self.session.application(), timeout);
        self.retain_observation("first product frame to reach surface present", result)
    }

    pub fn frame(&mut self) -> Result<ProbeFrame<S>> {
        let result = self.probe.read();
        self.retain_observation("current product observation", result)
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
        let result = self.probe.wait_checked(
            self.session.application(),
            timeout,
            description.clone(),
            |frame| condition.evaluate(&frame.state),
        );
        self.retain_observation(&description, result)
    }

    pub fn wait_stable<T: PartialEq>(
        &mut self,
        timeout: Duration,
        quiet: Duration,
        description: impl Into<String>,
        project: impl FnMut(&ProbeFrame<S>) -> Option<T>,
    ) -> Result<ProbeFrame<S>> {
        let description = description.into();
        let result = self.probe.wait_stable(
            self.session.application(),
            timeout,
            quiet,
            description.clone(),
            project,
        );
        self.retain_observation(&description, result)
    }

    pub fn anchor(&mut self, target: impl Display) -> Result<Anchor> {
        let name = target.to_string();
        let result = self
            .probe
            .wait_anchor(self.session.application(), &name, self.target_timeout);
        let anchor = self.retain_failure_frame(result)?;
        self.emit(StoryEvent::Fact(StoryFact::TargetResolved {
            target: &name,
            anchor: &anchor,
        }))?;
        Ok(anchor)
    }

    pub fn click(&mut self, target: impl Display) -> Result<Reaction<'_, 'app, 'bed, S, O>> {
        let target = target.to_string();
        let anchor = self.anchor(target.as_str())?;
        let receipt = self
            .session
            .click(anchor.center().0, anchor.center().1, Button::Primary)?;
        self.emit_action(&receipt, Some(&target), Some(anchor.center()))?;
        Ok(self.reaction_named(receipt, format!("click `{target}`")))
    }

    pub fn modified_click(
        &mut self,
        target: impl Display,
        button: Button,
        modifiers: Modifiers,
    ) -> Result<Reaction<'_, 'app, 'bed, S, O>> {
        let target = target.to_string();
        let anchor = self.anchor(target.as_str())?;
        let (x, y) = anchor.center();
        let receipt = self.session.modified_click(x, y, button, modifiers)?;
        self.emit_action(&receipt, Some(&target), Some((x, y)))?;
        Ok(self.reaction_named(receipt, format!("{modifiers:?} click `{target}`")))
    }

    pub fn click_anchor(&mut self, anchor: &Anchor) -> Result<Reaction<'_, 'app, 'bed, S, O>> {
        let (x, y) = anchor.center();
        let receipt = self.session.click(x, y, Button::Primary)?;
        self.emit_action(&receipt, Some(&anchor.name), Some((x, y)))?;
        Ok(self.reaction_named(receipt, format!("click `{}`", anchor.name)))
    }

    pub fn click_at(
        &mut self,
        point: (i16, i16),
        button: Button,
    ) -> Result<Reaction<'_, 'app, 'bed, S, O>> {
        let receipt = self.session.click(point.0, point.1, button)?;
        self.emit_action(&receipt, None, Some(point))?;
        Ok(self.reaction(receipt))
    }

    pub fn click_current(&mut self, button: Button) -> Result<Reaction<'_, 'app, 'bed, S, O>> {
        let point = self.session.pointer()?;
        self.click_at(point, button)
    }

    /// Glide to a named semantic target without pressing a pointer button.
    pub fn point(
        &mut self,
        target: impl Display,
        policy: Motion,
    ) -> Result<Reaction<'_, 'app, 'bed, S, O>> {
        let target = target.to_string();
        let destination = self.anchor(target.as_str())?.center();
        let receipt = self.session.motion(destination, policy)?;
        self.emit_action(&receipt, Some(&target), Some(destination))?;
        Ok(self.reaction_named(receipt, format!("point at `{target}`")))
    }

    pub fn motion_to(
        &mut self,
        destination: (i16, i16),
        policy: Motion,
    ) -> Result<Reaction<'_, 'app, 'bed, S, O>> {
        let receipt = self.session.motion(destination, policy)?;
        self.emit_action(&receipt, None, Some(destination))?;
        Ok(self.reaction(receipt))
    }

    /// Glide to a live target, then resolve it again before clicking.
    pub fn tap(
        &mut self,
        target: impl Display,
        button: Button,
        policy: Motion,
    ) -> Result<Reaction<'_, 'app, 'bed, S, O>> {
        let target = target.to_string();
        let _hovered = self.point(target.as_str(), policy)?.next_frame()?;
        let anchor = self.anchor(target.as_str())?;
        let point = anchor.center();
        let receipt = self.session.click(point.0, point.1, button)?;
        self.emit_action(&receipt, Some(&target), Some(point))?;
        Ok(self.reaction_named(receipt, format!("tap `{target}`")))
    }

    pub fn modified_click_at(
        &mut self,
        point: (i16, i16),
        button: Button,
        modifiers: Modifiers,
    ) -> Result<Reaction<'_, 'app, 'bed, S, O>> {
        let receipt = self
            .session
            .modified_click(point.0, point.1, button, modifiers)?;
        self.emit_action(&receipt, None, Some(point))?;
        Ok(self.reaction(receipt))
    }

    pub fn drag(
        &mut self,
        target: impl Display,
        destination: (i16, i16),
        policy: Drag,
    ) -> Result<Reaction<'_, 'app, 'bed, S, O>> {
        let target = target.to_string();
        let origin = self.anchor(target.as_str())?.center();
        let receipt = self.session.drag(origin, destination, policy)?;
        self.emit_action(&receipt, Some(&target), Some(destination))?;
        Ok(self.reaction_named(receipt, format!("drag `{target}`")))
    }

    pub fn drag_from(
        &mut self,
        origin: (i16, i16),
        destination: (i16, i16),
        policy: Drag,
    ) -> Result<Reaction<'_, 'app, 'bed, S, O>> {
        let receipt = self.session.drag(origin, destination, policy)?;
        self.emit_action(&receipt, None, Some(destination))?;
        Ok(self.reaction(receipt))
    }

    /// Drag between targets resolved on opposite sides of button acquisition.
    ///
    /// Resolving the destination after the press keeps the gesture correct when
    /// grabbing the source changes layout. Every error path attempts release.
    pub fn drag_to(
        &mut self,
        origin: impl Display,
        destination: impl Display,
        policy: Drag,
    ) -> Result<Reaction<'_, 'app, 'bed, S, O>> {
        let origin = origin.to_string();
        let destination = destination.to_string();
        let start = self.anchor(origin.as_str())?.center();
        let down = self.session.button_down(start.0, start.1, policy.button)?;
        let operation = (|| -> Result<(i16, i16)> {
            self.emit_action(&down, Some(&origin), Some(start))?;
            if !policy.press_duration.is_zero() {
                thread::sleep(policy.press_duration);
            }
            let _acquired = self
                .reaction_named(down, format!("acquire `{origin}`"))
                .next_frame()?;
            let end = self.anchor(destination.as_str())?.center();
            let motion = self.session.motion(
                end,
                Motion {
                    steps: policy.steps,
                    duration: policy.duration,
                },
            )?;
            self.emit_action(&motion, Some(&destination), Some(end))?;
            Ok(end)
        })();
        let released = self.session.button_up(policy.button);
        let end = operation?;
        let up = released?;
        self.emit_action(&up, Some(&destination), Some(end))?;
        Ok(self.reaction_named(up, format!("drag `{origin}` to `{destination}`")))
    }

    pub fn stroke(
        &mut self,
        knots: &[(i16, i16)],
        policy: Stroke,
    ) -> Result<Reaction<'_, 'app, 'bed, S, O>> {
        let receipt = self.session.stroke(knots, policy)?;
        self.emit_action(&receipt, None, knots.last().copied())?;
        Ok(self.reaction(receipt))
    }

    pub fn wheel(
        &mut self,
        point: (i16, i16),
        ticks: i32,
        policy: Wheel,
    ) -> Result<Reaction<'_, 'app, 'bed, S, O>> {
        let receipt = self.session.wheel(point.0, point.1, ticks, policy)?;
        self.emit_action(&receipt, None, Some(point))?;
        Ok(self.reaction(receipt))
    }

    pub fn modified_wheel(
        &mut self,
        point: (i16, i16),
        ticks: i32,
        policy: Wheel,
        modifiers: Modifiers,
    ) -> Result<Reaction<'_, 'app, 'bed, S, O>> {
        let receipt = self
            .session
            .modified_wheel(point.0, point.1, ticks, policy, modifiers)?;
        self.emit_action(&receipt, None, Some(point))?;
        Ok(self.reaction(receipt))
    }

    pub fn scroll(
        &mut self,
        point: (i16, i16),
        ticks: i32,
    ) -> Result<Reaction<'_, 'app, 'bed, S, O>> {
        let receipt = self.session.scroll(point.0, point.1, ticks)?;
        self.emit_action(&receipt, None, Some(point))?;
        Ok(self.reaction(receipt))
    }

    pub fn key(&mut self, key: Key) -> Result<Reaction<'_, 'app, 'bed, S, O>> {
        let receipt = self.session.key(key)?;
        self.emit_action(&receipt, None, None)?;
        Ok(self.reaction(receipt))
    }

    pub fn chord(
        &mut self,
        modifiers: Modifiers,
        key: Key,
    ) -> Result<Reaction<'_, 'app, 'bed, S, O>> {
        let receipt = self.session.chord(modifiers, key)?;
        self.emit_action(&receipt, None, None)?;
        Ok(self.reaction(receipt))
    }

    pub fn type_text(&mut self, text: &str) -> Result<Reaction<'_, 'app, 'bed, S, O>> {
        let receipt = self.session.type_text(text)?;
        self.emit_action(&receipt, None, None)?;
        Ok(self.reaction(receipt))
    }

    /// Replace one text target through the same focus, selection, and typing
    /// sequence used by a person.
    pub fn replace_text(
        &mut self,
        target: impl Display,
        text: &str,
        focused: Condition<S>,
    ) -> Result<Reaction<'_, 'app, 'bed, S, O>> {
        let _focused = self.click(target)?.until(focused)?;
        self.replace_focused_text(text)
    }

    /// Select and replace the contents of a text editor which already owns focus.
    pub fn replace_focused_text(&mut self, text: &str) -> Result<Reaction<'_, 'app, 'bed, S, O>> {
        let selected = self.session.chord(Modifiers::CTRL, Key::Character('a'))?;
        self.emit_action(&selected, None, None)?;
        self.type_text(text)
    }

    pub fn reaction(&mut self, receipt: ActionReceipt) -> Reaction<'_, 'app, 'bed, S, O> {
        let description = receipt.action().to_owned();
        self.reaction_named(receipt, description)
    }

    /// Emit an editorial chapter without changing ordinary test behavior.
    pub fn chapter(&mut self, title: &str) -> Result<()> {
        self.emit(StoryEvent::Cue(StoryCue::Chapter { title }))
    }

    /// Offer recording observers a live interval while ordinary tests proceed immediately.
    pub fn hold(&mut self, duration: Duration) -> Result<()> {
        self.emit(StoryEvent::Cue(StoryCue::Hold { duration }))
    }

    pub fn capture(&self) -> Result<Frame> {
        self.session.capture()
    }

    /// Seal the observer and return it to the caller.
    pub fn finish(mut self) -> Result<O> {
        self.observer.finish(StorySurface {
            surface: &self.session,
        })?;
        Ok(self.observer)
    }

    fn reaction_named(
        &mut self,
        receipt: ActionReceipt,
        description: String,
    ) -> Reaction<'_, 'app, 'bed, S, O> {
        let budget = self.reaction_budget;
        Reaction {
            story: self,
            receipt,
            budget,
            description,
        }
    }

    fn retain_failure_frame<T>(&self, result: Result<T>) -> Result<T> {
        if result.is_err() {
            let _capture = self.session.capture();
        }
        result
    }

    fn retain_observation(
        &mut self,
        description: &str,
        result: Result<ProbeFrame<S>>,
    ) -> Result<ProbeFrame<S>> {
        let frame = self.retain_failure_frame(result)?;
        self.emit_observation(description, &frame)?;
        Ok(frame)
    }

    fn emit_observation(&mut self, description: &str, frame: &ProbeFrame<S>) -> Result<()> {
        self.emit(StoryEvent::Fact(StoryFact::ObservationMatched {
            description,
            schema: frame.schema,
            frame: frame.frame,
            begun_ns: frame.begun_ns,
            observed_ns: frame.observed_ns,
            surface_presented_ns: frame.surface_presented_ns,
            surface_sequence: frame.surface_sequence,
        }))
    }

    fn emit_action(
        &mut self,
        receipt: &ActionReceipt,
        target: Option<&str>,
        pointer: Option<(i16, i16)>,
    ) -> Result<()> {
        self.emit(StoryEvent::Fact(StoryFact::ActionDispatched {
            action: receipt.action(),
            target,
            pointer: pointer.map(|(x, y)| [x, y]),
            gesture_started_ns: receipt.gesture_started_ns(),
            triggered_ns: receipt.triggered_ns(),
            completed_ns: receipt.completed_ns(),
        }))
    }

    fn emit(&mut self, event: StoryEvent<'_>) -> Result<()> {
        self.observer.observe(
            event,
            StorySurface {
                surface: &self.session,
            },
        )
    }
}

/// One injected gesture awaiting a temporally eligible semantic cue.
pub struct Reaction<'story, 'app, 'bed, S, O = Silent> {
    story: &'story mut Story<'app, 'bed, S, O>,
    receipt: ActionReceipt,
    budget: ReactionBudget,
    description: String,
}

impl<S: DeserializeOwned + 'static, O: StoryObserver> Reaction<'_, '_, '_, S, O> {
    #[must_use]
    pub fn within(&mut self, budget: ReactionBudget) -> &mut Self {
        self.budget = budget;
        self
    }

    pub const fn receipt(&self) -> &ActionReceipt {
        &self.receipt
    }

    /// Wait for a post-trigger witness condition.
    ///
    /// This fences subsequent external assertions; it does not itself prove
    /// that the gesture caused the observed state.
    pub fn until(&mut self, condition: Condition<S>) -> Result<Timed<ProbeFrame<S>>> {
        let description = format!("{}; await {}", self.description, condition.description());
        if self.budget.production().is_some() && !self.story.observer.permits_performance_verdicts()
        {
            return Err(crate::Error::Unsupported {
                capability: "story observation during performance adjudication",
                detail: "recording or tracing would contaminate the production latency verdict"
                    .to_owned(),
            });
        }
        let result = self.story.probe.wait_budgeted_checked(
            self.story.session.application(),
            &self.receipt,
            self.budget,
            description.clone(),
            |frame| condition.evaluate(&frame.state),
        );
        let timed = self.story.retain_failure_frame(result)?;
        self.story.emit_observation(&description, timed.value())?;
        Ok(timed)
    }

    pub fn next_frame(&mut self) -> Result<Timed<ProbeFrame<S>>> {
        self.until(Condition::new("a fresh surface-presented frame", |_| true))
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
