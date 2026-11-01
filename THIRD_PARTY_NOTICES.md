# Third-party notices

## DeepSeek Harness web UI

The initial web UI in `ui/` is derived from DeepSeek Harness:

- Source: https://github.com/deepseek-ai/deepseek-harness
- License: MIT
- Copyright (c) 2026 DeepSeek

MIT License

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

## ZCode terminal pane and task list

The Web terminal dock's tab model, dock layout and CSS-token theme resolution
in `ui/plugins/@xlang/xharness-client-ui-terminal/client.js` are adapted from
the Apache-2.0 `zai-org/ZCode` terminal pane; the task panel's pinned section,
timeline grouping, inline rename and action menu in
`ui/plugins/@xlang/xharness-client-ui-tasks/client.js` are adapted from its
task list. The transports and the XHarness integrations are original.

- Source: https://github.com/zai-org/ZCode (packages/ui/src/Terminal.tsx,
  packages/ui/src/terminal/)
- License: Apache License 2.0

                                 Apache License
                           Version 2.0, January 2004
                        http://www.apache.org/licenses/

   Licensed under the Apache License, Version 2.0 (the "License");
   you may not use this file except in compliance with the License.
   You may obtain a copy of the License at

       http://www.apache.org/licenses/LICENSE-2.0

   Unless required by applicable law or agreed to in writing, software
   distributed under the License is distributed on an "AS IS" BASIS,
   WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
   See the License for the specific language governing permissions and
   limitations under the License.

## xterm.js

The vendored terminal renderer at
`ui/plugins/@xlang/xharness-client-ui-terminal/vendor/xterm.js` (and its
stylesheet) is the unmodified @xterm/xterm 5.5.0 UMD build.

- Source: https://github.com/xtermjs/xterm.js
- License: MIT, see `vendor/LICENSE-xterm.txt`

Copyright (c) 2017-2019, The xterm.js authors (https://github.com/xtermjs/xterm.js)
Copyright (c) 2014-2016, SourceLair Private Company (https://www.sourcelair.com)
Copyright (c) 2012-2013, Christopher Jeffrey (https://github.com/chjj/)

## ZCode loading motion and Lucide Loader glyph

`ui/src/modules/conversation/chat/LoadingIndicator.tsx` and its stylesheet adapt
ZCode's ChatLoading visual behavior (HTML wrapper rotation, static SVG and fade-in)
to XHarness's own theme and authoritative turn state. No ZCode runtime is bundled.

- Source: https://github.com/zai-org/ZCode (Apache License 2.0)
- Loader glyph: Lucide, including Feather-derived icons (ISC / MIT)
- Full icon notices: `docs/licenses/lucide-loader.txt`; also preserved inline in
  the component and generated conversation bundle.

## DeepSeek SuperAgent product behavior port

The product-owned `xharness-superagent-core` and `xharness-superagent` crates port portions of
[deepseek-ai/dsh-superagent-prototype](https://github.com/deepseek-ai/dsh-superagent-prototype)
at commit `6337fc823ce20251c8de8085948165c81aaa701e`. The original MIT notice is
retained in each crate's `NOTICE` and in `conformance/reference/upstream/LICENSE`.

MIT License

Copyright (c) 2026 DeepSeek Harness Team

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


## V8 date parser grammar

The owned core date grammar adapts V8 13.6.233.10 dateparser source under BSD-3-Clause.


V8 date grammar Rust adaptation
Source: https://github.com/v8/v8/tree/13.6.233.10/src/date
Copyright 2011 the V8 project authors. All rights reserved.
Copyright 2014, the V8 project authors. All rights reserved.
Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are
met:

    * Redistributions of source code must retain the above copyright
      notice, this list of conditions and the following disclaimer.
    * Redistributions in binary form must reproduce the above
      copyright notice, this list of conditions and the following
      disclaimer in the documentation and/or other materials provided
      with the distribution.
    * Neither the name of Google Inc. nor the names of its
      contributors may be used to endorse or promote products derived
      from this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
"AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR
A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT
OWNER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

## Supplemental published skill tool SDK

`conformance/reference/skill-tool-sdk.mjs` is the unmodified published
`@deepseek-ai/dsh-tool-skill@0.2.1-alpha.2` JavaScript entrypoint.
Full MIT license, package integrity, source hash and provenance are retained in
`conformance/reference/skill-tool-sdk-source.json`. This supplemental package is
not independently pinned by the prototype baseline lock; no reference source
files or prototype lock have been changed to incorporate it.

## Supplemental full AgentLoop oracle packages

The optional offline conformance oracle fetches exact published
`dsh-agent-loop`, `dsh-session-projection`, `dsh-brand`, `dsh-util-values`,
`dsh-session-persistence-jsonl` (all `0.2.1-alpha.2`), and Linux x64
`node-addon-system` / prebuilt (`0.1.2`). They are not product dependencies or
an advance of the fixed prototype baseline. Tarball SHA-512/SHA-256, every
extracted file hash and full MIT/BSD-3-Clause notices are retained in
`conformance/reference/full-agent-loop-source.json`. No package scripts run.
