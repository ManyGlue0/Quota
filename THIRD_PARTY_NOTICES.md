# Third-party notices

Quota stands on other people's work. Thank you.

## OpenUsage

Quota is a Windows take on [OpenUsage](https://github.com/robinebers/openusage) by Robin Ebers. Its
provider research (endpoints, response mappings, login locations), the local spend scanners, the
pricing resolution and its pricing supplement (`src-tauri/resources/pricing/supplement.json`), the
pacing rules and the popover design are ported from it. Quota is **not** OpenUsage, not affiliated
with it and not endorsed by it; per OpenUsage's [trademark policy](https://github.com/robinebers/openusage/blob/main/TRADEMARK.md)
it uses its own name and mark.

```
MIT License

Copyright (c) 2026 Robin Ebers

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Model prices

- `src-tauri/resources/pricing/litellm.json`: a snapshot of `model_prices_and_context_window.json`
  from [LiteLLM](https://github.com/BerriAI/litellm) by Berri AI, MIT License.
- `src-tauri/resources/pricing/models_dev.json`: a snapshot of [models.dev](https://github.com/sst/models.dev)
  by SST, MIT License.

## Provider logos

The marks in `scripts/logos/` and `src/assets/providers/` identify each provider and are
trademarks of their owners (Anthropic, OpenAI, Anysphere, GitHub, Cognition, xAI, Meta, Ollama,
SST, OpenRouter, Z.ai, Google). Quota uses them only to show whose usage a row is about; it isn't
affiliated with or endorsed by any of them. The set comes from OpenUsage; the Meta mark (Muse Code)
comes from [Simple Icons](https://github.com/simple-icons/simple-icons), CC0 1.0.

## Holster

Quota is an app for [Holster](https://github.com/ManyGlue0/Holster) and follows its app contract and
design language (MIT License).
