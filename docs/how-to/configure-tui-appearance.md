# How to configure TUI appearance

Use the `/config` Settings overlay to select a built-in color scheme or create
a custom palette without restarting `neenee`.

## Apply a preset

1. Type `/config` and press `Enter`.
2. Select **Appearance** and press `Enter`.
3. Select `Zen`, `Midnight`, `Nord`, `Catppuccin`, or `Paper`.
4. Press `Enter` or `Space`.

The new scheme applies immediately and is saved to `config.toml`.

## Create a custom palette

1. Open `/config` → **Appearance**.
2. Select **Custom** and press `Enter`.
3. Use `Tab`, `Shift+Tab`, `↑`, or `↓` to move between semantic colors.
4. Press `Ctrl+U` to clear the current field, then enter a `#RRGGBB` value.
5. Press `Enter` to save and apply the complete palette.

Valid values preview across the interface while the editor is open. Press
`Esc` to discard the draft and restore the previously active scheme.

For preset names, custom field defaults, and the equivalent TOML schema, see
the [Configuration Reference](../reference/configuration.md#tui-presentation).
