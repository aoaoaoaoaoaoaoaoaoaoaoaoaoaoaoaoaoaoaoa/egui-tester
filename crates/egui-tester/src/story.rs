use std::{
    fmt::Display,
    num::NonZeroU16,
    thread,
    time::{Duration, Instant},
};

use serde::{Serialize, de::DeserializeOwned};

use crate::{
    ActionReceipt, Anchor, Application, Approach, ArtifactStem, Button, Condition, Cursor, Drag,
    Error, Frame, Framing, Key, Modifiers, Motion, Probe, ProbeFrame, ReactionBudget, Result,
    Stroke, Testbed, Timed, Wheel, WindowQuery, X11Session, x11::Rest,
};

/// Quiet a still's presentation keeps before its cue. Ambient animation under
/// the testbed's software renderer presents a pass every 100–250 ms, so a
/// transition still under way shows its next change well within this span.
const STILL_REST: Duration = Duration::from_millis(400);
/// Interval between the real pointer's steps on an approach glide.
const GLIDE_STEP: Duration = Duration::from_millis(8);

/// Persistent rate for observer-authored choreography.
///
/// The standard rate is one. Larger factors accelerate automatic beats while
/// explicit chapter and hold durations retain their authored wall time.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct StoryTempo(NonZeroU16);

impl StoryTempo {
    pub const STANDARD: Self = Self(NonZeroU16::MIN);
    pub const TWOFOLD: Self = Self::integer(2);
    pub const FOURFOLD: Self = Self::integer(4);
    pub const EIGHTFOLD: Self = Self::integer(8);

    #[must_use]
    pub const fn accelerated(factor: NonZeroU16) -> Self {
        Self(factor)
    }

    #[must_use]
    pub const fn factor(self) -> u16 {
        self.0.get()
    }

    const fn integer(factor: u16) -> Self {
        match NonZeroU16::new(factor) {
            Some(factor) => Self(factor),
            None => Self::STANDARD,
        }
    }
}

impl Default for StoryTempo {
    fn default() -> Self {
        Self::STANDARD
    }
}

/// Authored editorial instruction carried by a live story stream.
///
/// A `Still` names the presented surface at that instant as one publishable
/// still, unique within one take.
///
/// Cues, facts, and events are deliberately exhaustive: an observer meets a
/// new kind at a compile error, never in a wildcard arm.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(tag = "cue", rename_all = "snake_case")]
pub enum StoryCue<'a> {
    Chapter { title: &'a str },
    Hold { duration: Duration },
    Tempo { tempo: StoryTempo },
    Still { name: &'a ArtifactStem },
}

/// Immutable execution fact emitted while a story drives the product.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(tag = "fact", rename_all = "snake_case")]
pub enum StoryFact<'a> {
    TargetResolved {
        target: &'a str,
        anchor: &'a Anchor,
    },
    /// A pointer-bearing gesture has acquired its destination but has not yet
    /// touched the product. Recorders may therefore compose an approach
    /// without mistaking target reconnaissance for visible choreography.
    PointerAimed {
        target: Option<&'a str>,
        pointer: [i16; 2],
        anchor: Option<&'a Anchor>,
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

pub(crate) trait CaptureSurface {
    fn capture(&self) -> Result<Frame>;
    fn cursor(&self) -> Result<Option<Cursor>>;
}

impl CaptureSurface for X11Session<'_, '_> {
    fn capture(&self) -> Result<Frame> {
        self.capture_ephemeral()
    }

    fn cursor(&self) -> Result<Option<Cursor>> {
        X11Session::cursor(self).map(Some)
    }
}

/// Capture-only view of the private product surface offered to observers:
/// its pixels and the pointer the display server presents over them.
/// Neither read injects input or mutates the product.
#[derive(Clone, Copy)]
pub struct StorySurface<'a> {
    surface: &'a dyn CaptureSurface,
}

impl StorySurface<'_> {
    pub(crate) const fn new(surface: &dyn CaptureSurface) -> StorySurface<'_> {
        StorySurface { surface }
    }

    pub fn capture(self) -> Result<Frame> {
        self.surface.capture()
    }

    /// The presented pointer at this instant, or `None` on a projection that
    /// presents none, such as Android touch.
    pub fn cursor(self) -> Result<Option<Cursor>> {
        self.surface.cursor()
    }
}

/// Synchronous consumer of one executing story's typed event stream.
pub trait StoryObserver {
    fn observe(&mut self, event: StoryEvent<'_>, surface: StorySurface<'_>) -> Result<()>;

    /// Offered between journal polls while the story awaits the product, so
    /// a film keeps its clock through product work. It must return promptly
    /// and sample only what is due.
    fn awaiting(&mut self, _surface: StorySurface<'_>) -> Result<()> {
        Ok(())
    }

    /// Close one launch's segment of a take. The observer may continue the
    /// take through a relaunched story, so this must not seal it.
    fn finish(&mut self, _surface: StorySurface<'_>) -> Result<()> {
        Ok(())
    }

    /// The framing this observer's take demands. A desktop story imposes it
    /// on the product window when the observer attaches.
    fn framing(&self) -> Option<Framing> {
        None
    }

    /// How a desktop story's real pointer closes on each aimed destination.
    /// It is asked at every aim, so it may follow the tempo.
    fn approach(&self) -> Approach {
        Approach::INSTANT
    }

    /// Whether this observer is admissible while production latency is judged.
    fn permits_performance_verdicts(&self) -> bool {
        false
    }
}

/// Platform-neutral editorial surface of an executing product story.
///
/// Product-owned choreography traits extend this vocabulary with semantic
/// acts. Each platform projection then supplies its own target resolution,
/// native input, synchronization, and verdict machinery while observers
/// consume the resulting causal stream unchanged.
pub trait Choreography {
    fn chapter(&mut self, title: &str) -> Result<()>;

    fn hold(&mut self, duration: Duration) -> Result<()>;

    fn tempo(&mut self, tempo: StoryTempo) -> Result<()>;

    /// Name the presented surface as one publishable still. Every projection
    /// answers it, so an observer develops whatever the score names.
    fn still(&mut self, name: &ArtifactStem) -> Result<()>;
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
    /// The framing imposed on the product window, and the monotonic instant
    /// it was imposed.
    framing: Option<(Framing, u64)>,
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
            framing: None,
        })
    }

    /// Attach one optional consumer before executing the story, imposing its
    /// framing on the product window.
    pub fn with_observer<O: StoryObserver>(self, observer: O) -> Result<Story<'app, 'bed, S, O>> {
        if self.reaction_budget.production().is_some() && !observer.permits_performance_verdicts() {
            return Err(Error::Unsupported {
                capability: "story observation during performance adjudication",
                detail: "recording or tracing would contaminate the production latency verdict"
                    .to_owned(),
            });
        }
        let framing = match observer.framing() {
            Some(framing) => {
                self.session.frame(framing)?;
                Some((framing, egui_tester_witness::monotonic_ns()))
            }
            None => None,
        };
        Ok(Story {
            session: self.session,
            probe: self.probe,
            observer,
            reaction_budget: self.reaction_budget,
            target_timeout: self.target_timeout,
            wait_timeout: self.wait_timeout,
            framing,
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

    /// Wait for the product's first presented pass. Under an observer's
    /// framing, that is the first pass begun after the window was reframed,
    /// and the window must then hold the framing.
    pub fn ready(&mut self, timeout: Duration) -> Result<ProbeFrame<S>> {
        const DESCRIPTION: &str = "first product frame to reach surface present";
        let reframed_ns = self.framing.map_or(0, |(_, reframed_ns)| reframed_ns);
        let result = self.probe.wait_inspecting(
            self.session.application(),
            timeout,
            DESCRIPTION,
            |frame| (frame.begun_ns >= reframed_ns).then_some(()).ok_or(None),
            pace(&mut self.observer, &self.session),
        );
        let frame = self.retain_failure_frame(result)?;
        if let Some((framing, _)) = self.framing {
            let [width, height] = self.session.geometry()?;
            if !framing.frames(width, height) {
                return Err(Error::Framing {
                    framing,
                    width,
                    height,
                });
            }
        }
        self.emit_observation(DESCRIPTION, &frame)?;
        Ok(frame)
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
        let result = self.probe.wait_inspecting(
            self.session.application(),
            timeout,
            description.clone(),
            |frame| condition.evaluate(&frame.state).map_err(Some),
            pace(&mut self.observer, &self.session),
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
        let result = self.probe.wait_stable_paced(
            self.session.application(),
            timeout,
            quiet,
            description.clone(),
            project,
            pace(&mut self.observer, &self.session),
        );
        self.retain_observation(&description, result)
    }

    pub fn anchor(&mut self, target: impl Display) -> Result<Anchor> {
        let name = target.to_string();
        let result = self.probe.wait_anchor_paced(
            self.session.application(),
            &name,
            self.target_timeout,
            pace(&mut self.observer, &self.session),
        );
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
        let point = anchor.center();
        self.aim(Some(&target), point, Some(&anchor))?;
        let receipt = self.session.click(point.0, point.1, Button::Primary)?;
        self.emit_action(&receipt, Some(&target), Some(point))?;
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
        self.aim(Some(&target), (x, y), Some(&anchor))?;
        let receipt = self.session.modified_click(x, y, button, modifiers)?;
        self.emit_action(&receipt, Some(&target), Some((x, y)))?;
        Ok(self.reaction_named(receipt, format!("{modifiers:?} click `{target}`")))
    }

    pub fn click_anchor(&mut self, anchor: &Anchor) -> Result<Reaction<'_, 'app, 'bed, S, O>> {
        let (x, y) = anchor.center();
        self.aim(Some(anchor.name.as_str()), (x, y), Some(anchor))?;
        let receipt = self.session.click(x, y, Button::Primary)?;
        self.emit_action(&receipt, Some(anchor.name.as_str()), Some((x, y)))?;
        Ok(self.reaction_named(receipt, format!("click `{}`", anchor.name)))
    }

    pub fn click_at(
        &mut self,
        point: (i16, i16),
        button: Button,
    ) -> Result<Reaction<'_, 'app, 'bed, S, O>> {
        self.aim(None, point, None)?;
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
        let anchor = self.anchor(target.as_str())?;
        let destination = anchor.center();
        self.sight(Some(&target), destination, Some(&anchor))?;
        let receipt =
            self.paced(|session, rest| session.motion_paced(destination, policy, rest))?;
        self.emit_action(&receipt, Some(&target), Some(destination))?;
        Ok(self.reaction_named(receipt, format!("point at `{target}`")))
    }

    pub fn motion_to(
        &mut self,
        destination: (i16, i16),
        policy: Motion,
    ) -> Result<Reaction<'_, 'app, 'bed, S, O>> {
        self.sight(None, destination, None)?;
        let receipt =
            self.paced(|session, rest| session.motion_paced(destination, policy, rest))?;
        self.emit_action(&receipt, None, Some(destination))?;
        Ok(self.reaction(receipt))
    }

    /// Withdraw the pointer to the screen corner farthest from the product
    /// window, so no hover state survives into a still and a film glides the
    /// pointer out of frame. Observers learn the departure point in window
    /// coordinates. Android has no pointer to withdraw, so this is no
    /// [`Choreography`] act.
    pub fn leave(&mut self) -> Result<Reaction<'_, 'app, 'bed, S, O>> {
        let departure = self.session.departure()?;
        self.aim(None, departure, None)?;
        let receipt = self.session.depart(departure)?;
        self.emit_action(&receipt, None, Some(departure))?;
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
        self.aim(Some(&target), point, Some(&anchor))?;
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
        self.aim(None, point, None)?;
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
        let anchor = self.anchor(target.as_str())?;
        let origin = anchor.center();
        self.aim(Some(&target), origin, Some(&anchor))?;
        let receipt =
            self.paced(|session, rest| session.drag_paced(origin, destination, policy, rest))?;
        self.emit_action(&receipt, Some(&target), Some(destination))?;
        Ok(self.reaction_named(receipt, format!("drag `{target}`")))
    }

    pub fn drag_from(
        &mut self,
        origin: (i16, i16),
        destination: (i16, i16),
        policy: Drag,
    ) -> Result<Reaction<'_, 'app, 'bed, S, O>> {
        self.aim(None, origin, None)?;
        let receipt =
            self.paced(|session, rest| session.drag_paced(origin, destination, policy, rest))?;
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
        let origin_anchor = self.anchor(origin.as_str())?;
        let start = origin_anchor.center();
        self.aim(Some(&origin), start, Some(&origin_anchor))?;
        let down = self.session.button_down(start.0, start.1, policy.button)?;
        let operation = (|| -> Result<(i16, i16)> {
            self.emit_action(&down, Some(&origin), Some(start))?;
            linger(&mut self.observer, &self.session, policy.press_duration)?;
            let _acquired = self
                .reaction_named(down, format!("acquire `{origin}`"))
                .next_frame()?;
            let destination_anchor = self.anchor(destination.as_str())?;
            let end = destination_anchor.center();
            self.sight(Some(&destination), end, Some(&destination_anchor))?;
            let transport = Motion {
                steps: policy.steps,
                duration: policy.duration,
            };
            let motion = self.paced(|session, rest| session.motion_paced(end, transport, rest))?;
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
        if let Some(&origin) = knots.first() {
            self.aim(None, origin, None)?;
        }
        let receipt = self.paced(|session, rest| session.stroke_paced(knots, policy, rest))?;
        self.emit_action(&receipt, None, knots.last().copied())?;
        Ok(self.reaction(receipt))
    }

    pub fn wheel(
        &mut self,
        point: (i16, i16),
        ticks: i32,
        policy: Wheel,
    ) -> Result<Reaction<'_, 'app, 'bed, S, O>> {
        self.aim(None, point, None)?;
        let receipt = self.paced(|session, rest| {
            session.wheel_paced(point.0, point.1, ticks, policy, Modifiers::empty(), rest)
        })?;
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
        self.aim(None, point, None)?;
        let receipt = self.paced(|session, rest| {
            session.wheel_paced(point.0, point.1, ticks, policy, modifiers, rest)
        })?;
        self.emit_action(&receipt, None, Some(point))?;
        Ok(self.reaction(receipt))
    }

    pub fn scroll(
        &mut self,
        point: (i16, i16),
        ticks: i32,
    ) -> Result<Reaction<'_, 'app, 'bed, S, O>> {
        self.aim(None, point, None)?;
        let receipt = self.paced(|session, rest| {
            session.wheel_paced(
                point.0,
                point.1,
                ticks,
                Wheel {
                    tick_duration: Duration::ZERO,
                },
                Modifiers::empty(),
                rest,
            )
        })?;
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

    /// Set the persistent tempo consumed by choreography observers.
    pub fn tempo(&mut self, tempo: StoryTempo) -> Result<()> {
        self.emit(StoryEvent::Cue(StoryCue::Tempo { tempo }))
    }

    /// Offer observers the presented surface as the publishable still `name`
    /// once the witnessed presentation rests: for 400 ms no presented pass
    /// moves an anchor or changes the product state. Ambient animation that
    /// changes neither continues into the still. The rest is an editorial
    /// guard over witnessed passes, never a synchronization verdict, and a
    /// presentation that never rests fails the story with what kept it moving.
    pub fn still(&mut self, name: &ArtifactStem) -> Result<()> {
        let description = format!("presentation at rest for still `{name}`");
        let result = self.probe.wait_rest_paced(
            self.session.application(),
            self.wait_timeout,
            STILL_REST,
            description.clone(),
            pace(&mut self.observer, &self.session),
        );
        let _rested = self.retain_observation(&description, result)?;
        self.emit(StoryEvent::Cue(StoryCue::Still { name }))
    }

    pub fn capture(&self) -> Result<Frame> {
        self.session.capture()
    }

    /// Close this launch's segment and return the observer, which may carry
    /// the same take into a relaunched story.
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

    /// Announce a pointer-bearing gesture's destination, then close on it as
    /// the observer's approach asks.
    fn aim(
        &mut self,
        target: Option<&str>,
        pointer: (i16, i16),
        anchor: Option<&Anchor>,
    ) -> Result<()> {
        self.sight(target, pointer, anchor)?;
        self.approach(pointer)
    }

    /// Announce a destination the gesture's own transport reaches.
    fn sight(
        &mut self,
        target: Option<&str>,
        pointer: (i16, i16),
        anchor: Option<&Anchor>,
    ) -> Result<()> {
        self.emit(StoryEvent::Fact(StoryFact::PointerAimed {
            target,
            pointer: [pointer.0, pointer.1],
            anchor,
        }))
    }

    /// Glide the real pointer to `destination`, then dwell there, both paced.
    fn approach(&mut self, destination: (i16, i16)) -> Result<()> {
        let Approach { glide, dwell } = self.observer.approach();
        if !glide.is_zero() && self.session.pointer()? != destination {
            let steps = u16::try_from(glide.as_millis() / GLIDE_STEP.as_millis())
                .unwrap_or(u16::MAX)
                .max(1);
            let glide = Motion {
                steps,
                duration: glide,
            };
            let _glided =
                self.paced(|session, rest| session.motion_paced(destination, glide, rest))?;
        }
        linger(&mut self.observer, &self.session, dwell)
    }

    /// Run one native gesture whose deliberate rests offer the observer the
    /// surface, so a film keeps its clock through input pacing.
    fn paced<T>(
        &mut self,
        gesture: impl FnOnce(&X11Session<'app, 'bed>, Rest<'_>) -> Result<T>,
    ) -> Result<T> {
        let (observer, session) = (&mut self.observer, &self.session);
        gesture(session, &mut |duration| linger(observer, session, duration))
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

impl<S: DeserializeOwned + 'static, O: StoryObserver> Choreography for Story<'_, '_, S, O> {
    fn chapter(&mut self, title: &str) -> Result<()> {
        Self::chapter(self, title)
    }

    fn hold(&mut self, duration: Duration) -> Result<()> {
        Self::hold(self, duration)
    }

    fn tempo(&mut self, tempo: StoryTempo) -> Result<()> {
        Self::tempo(self, tempo)
    }

    fn still(&mut self, name: &ArtifactStem) -> Result<()> {
        Self::still(self, name)
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
            return Err(Error::Unsupported {
                capability: "story observation during performance adjudication",
                detail: "recording or tracing would contaminate the production latency verdict"
                    .to_owned(),
            });
        }
        let result = self.story.probe.wait_budgeted_paced(
            self.story.session.application(),
            &self.receipt,
            self.budget,
            description.clone(),
            |frame| condition.evaluate(&frame.state),
            pace(&mut self.story.observer, &self.story.session),
        );
        let timed = self.story.retain_failure_frame(result)?;
        self.story.emit_observation(&description, timed.value())?;
        Ok(timed)
    }

    pub fn next_frame(&mut self) -> Result<Timed<ProbeFrame<S>>> {
        self.until(Condition::new("a fresh surface-presented frame", |_| true))
    }
}

/// The between-polls step of a story wait: offer the observer the surface,
/// then rest one poll interval.
fn pace<'s, O: StoryObserver>(
    observer: &'s mut O,
    session: &'s X11Session<'_, '_>,
) -> impl FnMut() -> Result<()> + 's {
    move || {
        observer.awaiting(StorySurface { surface: session })?;
        crate::probe::rest()
    }
}

/// Rest `duration`, offering the observer the surface every poll meanwhile.
fn linger<O: StoryObserver>(
    observer: &mut O,
    session: &X11Session<'_, '_>,
    duration: Duration,
) -> Result<()> {
    let end = Instant::now() + duration;
    loop {
        observer.awaiting(StorySurface { surface: session })?;
        match end.checked_duration_since(Instant::now()) {
            Some(left) if !left.is_zero() => thread::sleep(left.min(crate::probe::POLL)),
            _ => return Ok(()),
        }
    }
}

/// Turn an external product fact into a first-class acceptance verdict.
pub fn demand(condition: bool, detail: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Error::Verdict {
            detail: detail.into(),
        })
    }
}
