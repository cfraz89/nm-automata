//! Reading an empty string as an absent value.

/// Read an empty string as an absent value.
///
/// D-Bus has no null, so a property that has no value has to send something, and for a good
/// many of NetworkManager's it sends `""`. A device with no permanent hardware address reports
/// one that way, as does a Wi-Fi P2P device asked for its `HwAddress`:
///
/// ```no_run
/// # use nm_automata::{NonEmpty, WirelessProxy};
/// # async fn example(wireless: WirelessProxy<'_>) -> zbus::Result<()> {
/// match wireless.perm_hw_address().await?.non_empty() {
///     Some(address) => println!("permanent address {address}"),
///     None => println!("no permanent address"),
/// }
/// # Ok(())
/// # }
/// ```
///
/// # Why this is yours to call
///
/// It would be tidier for the generated getters to return [`Option`] and save you the trouble.
/// They do not, because nothing upstream says which properties mean it. The introspection XML
/// carries no nullability annotation at all, and `NM-1.0.gir` marks none of its properties
/// nullable — so the crate would be asserting from guesswork what it otherwise only ever
/// reports from the spec.
///
/// It would also be wrong often enough to matter. Of the string-typed settings properties that
/// document their empty behaviour, hardly any mean "absent": an empty `connection.permissions`
/// grants **every** user access to a profile, and an empty `802-1x.altsubject-matches` disables
/// certificate verification entirely. Reading either as "unset" inverts it.
///
/// So the getters hand back exactly what NetworkManager sent, and this turns it into an
/// [`Option`] where *you* know that is what it meant. Nothing is lost either way: an empty
/// string can still become [`None`], but a [`None`] could never have told you which of those
/// meanings it started as.
pub trait NonEmpty: Sized {
    /// `Some(self)`, or [`None`] if it is empty.
    fn non_empty(self) -> Option<Self>;
}

impl NonEmpty for String {
    fn non_empty(self) -> Option<Self> {
        (!self.is_empty()).then_some(self)
    }
}

impl NonEmpty for &str {
    fn non_empty(self) -> Option<Self> {
        (!self.is_empty()).then_some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::NonEmpty;

    #[test]
    fn empty_is_absent_and_everything_else_is_kept() {
        assert_eq!(String::new().non_empty(), None);
        assert_eq!("".non_empty(), None);
        assert_eq!("4C:79:6E:DA:29:47".non_empty(), Some("4C:79:6E:DA:29:47"));
        // Whitespace is a value; only genuinely empty is read as absent.
        assert_eq!(" ".non_empty(), Some(" "));
    }
}
