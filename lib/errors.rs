//! Utility types and functions for errors

use fatality::{Fatality, Split};
use thiserror::Error;

/// Display an error with causes.
/// This is useful for displaying errors without converting to
/// `miette::Report` or `anyhow::Error` first
pub struct ErrorChain<'a>(&'a (dyn std::error::Error));

impl<'a> ErrorChain<'a> {
    pub fn new<E>(err: &'a E) -> Self
    where
        E: std::error::Error,
    {
        Self(err)
    }
}

impl std::fmt::Display for ErrorChain<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)?;
        let mut source: Option<&dyn std::error::Error> = self.0.source();
        while let Some(cause) = source {
            std::fmt::Display::fmt(": ", f)?;
            std::fmt::Display::fmt(cause, f)?;
            source = cause.source();
        }
        Ok(())
    }
}

/// Transparent wrapper around an error that allows it to be split into unboxed
/// errors.
/// This is primarily useful when boxed.
#[derive(Debug, Error)]
#[error(transparent)]
#[repr(transparent)]
pub struct Splittable<Err>(#[from] pub Err);

impl<Err> Fatality for Splittable<Err>
where
    Err: Fatality,
{
    fn is_fatal(&self) -> bool {
        self.0.is_fatal()
    }
}

impl<Err> Split for Splittable<Err>
where
    Err: Split,
{
    type Fatal = Err::Fatal;

    type Jfyi = Err::Jfyi;

    fn split(self) -> Result<Self::Jfyi, Self::Fatal> {
        self.0.split()
    }
}

impl<Err> Fatality for Box<Splittable<Err>>
where
    Err: Fatality,
{
    fn is_fatal(&self) -> bool {
        <Splittable<Err> as Fatality>::is_fatal(self)
    }
}

impl<Err> Split for Box<Splittable<Err>>
where
    Err: Split,
{
    type Fatal = <Splittable<Err> as Split>::Fatal;

    type Jfyi = <Splittable<Err> as Split>::Jfyi;

    fn split(self) -> Result<Self::Jfyi, Self::Fatal> {
        <Splittable<Err> as Split>::split(*self)
    }
}
