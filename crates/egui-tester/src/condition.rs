use std::{
    fmt::{Debug, Formatter},
    ops::{BitAnd, BitOr, Not},
};

type Judge<S> = dyn Fn(&S) -> Result<(), String>;

/// Reified semantic predicate with a diagnostic rejection.
pub struct Condition<S> {
    description: String,
    judge: Box<Judge<S>>,
}

impl<S> Debug for Condition<S> {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("Condition")
            .field(&self.description)
            .finish()
    }
}

impl<S: 'static> Condition<S> {
    #[must_use]
    pub fn new(description: impl Into<String>, predicate: impl Fn(&S) -> bool + 'static) -> Self {
        let description = description.into();
        let rejection = description.clone();
        Self::diagnostic(description, move |state| {
            predicate(state)
                .then_some(())
                .ok_or_else(|| rejection.clone())
        })
    }

    #[must_use]
    pub fn diagnostic(
        description: impl Into<String>,
        judge: impl Fn(&S) -> Result<(), String> + 'static,
    ) -> Self {
        Self {
            description: description.into(),
            judge: Box::new(judge),
        }
    }

    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn evaluate(&self, state: &S) -> Result<(), String> {
        (self.judge)(state)
    }

    #[must_use]
    pub fn contramap<T: 'static>(
        self,
        project: impl for<'a> Fn(&'a T) -> &'a S + 'static,
    ) -> Condition<T> {
        let Self { description, judge } = self;
        Condition::diagnostic(description, move |outer| judge(project(outer)))
    }

    #[must_use]
    pub fn and(self, other: Self) -> Self {
        let description = format!("{} and {}", self.description, other.description);
        Self::diagnostic(description, move |state| {
            let left = self.evaluate(state);
            let right = other.evaluate(state);
            match (left, right) {
                (Ok(()), Ok(())) => Ok(()),
                (Err(left), Ok(())) => Err(left),
                (Ok(()), Err(right)) => Err(right),
                (Err(left), Err(right)) => Err(format!("{left}; {right}")),
            }
        })
    }

    #[must_use]
    pub fn or(self, other: Self) -> Self {
        let description = format!("{} or {}", self.description, other.description);
        Self::diagnostic(description, move |state| match self.evaluate(state) {
            Ok(()) => Ok(()),
            Err(left) => other
                .evaluate(state)
                .map_err(|right| format!("neither alternative held: {left}; {right}")),
        })
    }
}

impl<S: 'static> BitAnd for Condition<S> {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self::Output {
        self.and(rhs)
    }
}

impl<S: 'static> BitOr for Condition<S> {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        self.or(rhs)
    }
}

impl<S: 'static> Not for Condition<S> {
    type Output = Self;

    fn not(self) -> Self::Output {
        let description = format!("not {}", self.description);
        Self::diagnostic(description.clone(), move |state| {
            self.evaluate(state)
                .is_err()
                .then_some(())
                .ok_or_else(|| description.clone())
        })
    }
}

/// A named projection from semantic state.
pub struct Field<S, V> {
    name: String,
    project: Box<dyn Fn(&S) -> V>,
}

impl<S: 'static, V: Debug + 'static> Field<S, V> {
    #[must_use]
    pub fn satisfies(
        self,
        expectation: impl Into<String>,
        predicate: impl Fn(&V) -> bool + 'static,
    ) -> Condition<S> {
        let expectation = expectation.into();
        let description = format!("{} {expectation}", self.name);
        Condition::diagnostic(description, move |state| {
            let actual = (self.project)(state);
            predicate(&actual)
                .then_some(())
                .ok_or_else(|| format!("{} expected {expectation}, observed {actual:?}", self.name))
        })
    }
}

impl<S: 'static, V: Debug + PartialEq + 'static> Field<S, V> {
    #[must_use]
    pub fn eq(self, expected: V) -> Condition<S> {
        let description = format!("{} == {expected:?}", self.name);
        Condition::diagnostic(description, move |state| {
            let actual = (self.project)(state);
            (actual == expected)
                .then_some(())
                .ok_or_else(|| format!("{} expected {expected:?}, observed {actual:?}", self.name))
        })
    }

    #[must_use]
    pub fn ne(self, rejected: V) -> Condition<S> {
        let description = format!("{} != {rejected:?}", self.name);
        Condition::diagnostic(description, move |state| {
            let actual = (self.project)(state);
            (actual != rejected)
                .then_some(())
                .ok_or_else(|| format!("{} unexpectedly remained {actual:?}", self.name))
        })
    }
}

#[must_use]
pub fn field<S, V>(name: impl Into<String>, project: impl Fn(&S) -> V + 'static) -> Field<S, V> {
    Field {
        name: name.into(),
        project: Box::new(project),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct State {
        view: &'static str,
        count: usize,
    }

    #[test]
    fn conjunction_reports_every_rejected_field() {
        let condition = field("view", |state: &State| state.view).eq("edit")
            & field("count", |state: &State| state.count).eq(2);
        let rejection = condition
            .evaluate(&State {
                view: "browse",
                count: 1,
            })
            .expect_err("both fields must reject");
        assert!(rejection.contains("view expected"));
        assert!(rejection.contains("count expected"));
    }

    #[test]
    fn disjunction_accepts_either_branch() {
        let condition = field("view", |state: &State| state.view).eq("edit")
            | field("count", |state: &State| state.count).eq(1);
        assert!(
            condition
                .evaluate(&State {
                    view: "browse",
                    count: 1,
                })
                .is_ok()
        );
    }
}
