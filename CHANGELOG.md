# Changelog

## 0.1.0 (2026-11-25)


### Features

* a daily summary, langspan today (H4) ([#116](https://github.com/Langspan/LangSpan.dev/issues/116)) ([8d32192](https://github.com/Langspan/LangSpan.dev/commit/8d3219241d572a28a1f4cf9a39e2dd018761206f))
* a LangSpan skill that teaches agents how to work with it ([#100](https://github.com/Langspan/LangSpan.dev/issues/100)) ([c95b6c3](https://github.com/Langspan/LangSpan.dev/commit/c95b6c34c35fb1398ada1968bb3dd99309169139))
* a status line in Claude Code, sessions here and refusals today (H2) ([#122](https://github.com/Langspan/LangSpan.dev/issues/122)) ([7171a35](https://github.com/Langspan/LangSpan.dev/commit/7171a356e1274d47cabc7dad76e0cb952af416f8))
* look up a symbol with its calls, callers, digest and notes ([#104](https://github.com/Langspan/LangSpan.dev/issues/104)) ([0cc300b](https://github.com/Langspan/LangSpan.dev/commit/0cc300b9ba8d6be5662205b4a8415812b90fe958)), closes [#50](https://github.com/Langspan/LangSpan.dev/issues/50)
* notice agent hosts installed after LangSpan (A7) ([#133](https://github.com/Langspan/LangSpan.dev/issues/133)) ([d25b932](https://github.com/Langspan/LangSpan.dev/commit/d25b932d7d7d9f56835b0d1427e6ce642559e895))
* report where the source index is blind (D8) ([#113](https://github.com/Langspan/LangSpan.dev/issues/113)) ([917e4e4](https://github.com/Langspan/LangSpan.dev/commit/917e4e468a1b94bcd0250e0d81dc2a3035190081))
* search remembered notes with langspan recall (E2) ([#137](https://github.com/Langspan/LangSpan.dev/issues/137)) ([ed88996](https://github.com/Langspan/LangSpan.dev/commit/ed889963cdee2e7871dfa700f1a4d955b2f3d8dd))
* show a change's blast radius level by level with graph impact --depth (D2) ([#144](https://github.com/Langspan/LangSpan.dev/issues/144)) ([84fb497](https://github.com/Langspan/LangSpan.dev/commit/84fb497d1ca3ef038cc09a8132f9ff1ecb9fb736))
* status --mine, and a regression test for a session's new file (two sessions, skill only) ([#109](https://github.com/Langspan/LangSpan.dev/issues/109)) ([bb75626](https://github.com/Langspan/LangSpan.dev/commit/bb75626f33951522360530d68b460e34fd154c35))
* tell two sessions their tasks point at the same code before either writes ([#112](https://github.com/Langspan/LangSpan.dev/issues/112)) ([fdbb686](https://github.com/Langspan/LangSpan.dev/commit/fdbb686647365080a148f5c157e1a950fd951999)), closes [#45](https://github.com/Langspan/LangSpan.dev/issues/45)


### Bug fixes

* a long shell command no longer claims edits other sessions make while it runs ([#6](https://github.com/Langspan/LangSpan.dev/issues/6)) ([dd3a4c7](https://github.com/Langspan/LangSpan.dev/commit/dd3a4c761c7e6d1f1858c6de08114ec73d8a441e))
* a reformat keeps each line's author, so commit --mine works after a shared-checkout formatter run ([#154](https://github.com/Langspan/LangSpan.dev/issues/154)) ([a811329](https://github.com/Langspan/LangSpan.dev/commit/a811329746da3918968a32831394e27640208ba5))
* bound status and summaries (two sessions in one checkout) ([#102](https://github.com/Langspan/LangSpan.dev/issues/102)) ([a7e21f6](https://github.com/Langspan/LangSpan.dev/commit/a7e21f69ddf8a84b415bb3cae7c9b95ac3932daa))
* commit --mine attribution after formatters and unknown writes (two sessions in one checkout) ([#103](https://github.com/Langspan/LangSpan.dev/issues/103)) ([75ecd1f](https://github.com/Langspan/LangSpan.dev/commit/75ecd1f55df800c650e16a43efe394447a3f34af))
* commit --mine splits adjacent hunks and keeps a formatter's own lines (two sessions, skill only) ([#127](https://github.com/Langspan/LangSpan.dev/issues/127)) ([1321b16](https://github.com/Langspan/LangSpan.dev/commit/1321b16cf92703fbacb94bcaa1fbd631d6a880ac))
* coordinate Codex in a linked worktree with its own install ([#5](https://github.com/Langspan/LangSpan.dev/issues/5)) ([843da58](https://github.com/Langspan/LangSpan.dev/commit/843da58b2de1b64a25712ceb0cc5dd5ac8a10158))
* describe a second same-named definition as added, not as unchanged ([#138](https://github.com/Langspan/LangSpan.dev/issues/138)) ([c7d3571](https://github.com/Langspan/LangSpan.dev/commit/c7d35714c89886e16a21423852b696cc8921cc3f)), closes [#118](https://github.com/Langspan/LangSpan.dev/issues/118)
* doctor takes a state directory reached through a link for the same one ([#153](https://github.com/Langspan/LangSpan.dev/issues/153)) ([ef973db](https://github.com/Langspan/LangSpan.dev/commit/ef973db73600d112cb25f771df92003767617ecc))
* explain inconclusive MCP checks and relay Git errors from commit --mine ([#7](https://github.com/Langspan/LangSpan.dev/issues/7)) ([135cfdf](https://github.com/Langspan/LangSpan.dev/commit/135cfdf664dd4254e30a346be3285b5117e9851b))
* give each session from one prompt template its own task line, and tell sessions when a workspace-wide formatter rewrote their files ([#142](https://github.com/Langspan/LangSpan.dev/issues/142)) ([d8ce38f](https://github.com/Langspan/LangSpan.dev/commit/d8ce38fbe37bcd4e41b9f07ce0aa1c8972829651))
* keep every MCP tool reply within one frame ([#152](https://github.com/Langspan/LangSpan.dev/issues/152)) ([7be0701](https://github.com/Langspan/LangSpan.dev/commit/7be0701a5002faa34ec6b7afdcfe4f44f234995f)), closes [#143](https://github.com/Langspan/LangSpan.dev/issues/143)
* keep one list of the MCP tools instead of hand-written counts ([#119](https://github.com/Langspan/LangSpan.dev/issues/119)) ([c1649de](https://github.com/Langspan/LangSpan.dev/commit/c1649deb25b28c25d3afb01057be4569f884f66a)), closes [#115](https://github.com/Langspan/LangSpan.dev/issues/115)
* name the session and shell command when another session's formatter changed a file ([#132](https://github.com/Langspan/LangSpan.dev/issues/132)) ([011d9d2](https://github.com/Langspan/LangSpan.dev/commit/011d9d2800339137f6f22ed9b85dbef451fd47c8)), closes [#107](https://github.com/Langspan/LangSpan.dev/issues/107)
* redact a check's recorded command line ([#10](https://github.com/Langspan/LangSpan.dev/issues/10)) ([3aad52b](https://github.com/Langspan/LangSpan.dev/commit/3aad52bf5716599b7859be98600623ac9d59c22f))
* run release checks on the component release branch ([#123](https://github.com/Langspan/LangSpan.dev/issues/123)) ([326c63b](https://github.com/Langspan/LangSpan.dev/commit/326c63b55d82abe7f45083795d32c28eddc3ee4a))
* uninstall removes a hooks file left holding only LangSpan's entries ([#148](https://github.com/Langspan/LangSpan.dev/issues/148)) ([3a80b89](https://github.com/Langspan/LangSpan.dev/commit/3a80b89a5508f49303f0b8aed6d4821bfcb44962))
