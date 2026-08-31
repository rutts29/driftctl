//! Explicit in-session control grammar for the Codex plugin.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PluginControl {
    On,
    Off,
    Status,
}

pub(crate) fn parse(prompt: &str) -> Option<PluginControl> {
    let mut fields = prompt.split_whitespace();
    if !matches!(fields.next()?, "$driftctl" | "$driftctl-codex:driftctl") {
        return None;
    }
    let action = match fields.next()? {
        "on" => PluginControl::On,
        "off" => PluginControl::Off,
        "status" => PluginControl::Status,
        _ => return None,
    };
    fields.next().is_none().then_some(action)
}

#[cfg(test)]
mod tests {
    use super::{PluginControl, parse};

    #[test]
    fn accepts_only_the_explicit_two_token_control_grammar() {
        assert_eq!(parse("$driftctl on"), Some(PluginControl::On));
        assert_eq!(parse("  $driftctl\tstatus\n"), Some(PluginControl::Status));
        assert_eq!(parse("$driftctl off"), Some(PluginControl::Off));
        assert_eq!(
            parse("$driftctl-codex:driftctl on"),
            Some(PluginControl::On)
        );
        assert_eq!(
            parse("$driftctl-codex:driftctl status"),
            Some(PluginControl::Status)
        );
        assert_eq!(
            parse("$driftctl-codex:driftctl off"),
            Some(PluginControl::Off)
        );

        for prompt in [
            "$driftctl",
            "$driftctl ON",
            "$driftctl on now",
            "please use $driftctl on",
            "`$driftctl on`",
            "$driftctl-on",
            "$other:driftctl on",
            "$driftctl-codex:driftctl on now",
        ] {
            assert_eq!(parse(prompt), None, "unexpected control match: {prompt}");
        }
    }
}
