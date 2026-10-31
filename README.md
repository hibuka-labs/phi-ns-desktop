The `desk.*` capability pack: the desktop face.

What the model writes is code against `desk.*`:

```python
state = desk.win_state()
for el in state.els:
    if el.text == "Save":
        el.click()
        break
shot = desk.shot()              # -> ImageHandle
```

The pack speaks only the `phi-ns` contract over a `Transport`; assembly and
policy live above it. The wire is `phi-wire-protocol` with the declared
method name as `op` — one vocabulary, nothing to translate through.

The pack is deliberately thin: target normalization for the worker, the
method-to-wire mapping, and nothing else.

```toml
[dependencies]
phi-ns-desktop = "0.1.0"
```

## License

MIT.
