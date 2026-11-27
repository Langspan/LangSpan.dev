# Changelog

## [0.3.0](https://github.com/Langspan/LangSpan.dev/compare/v0.2.0...v0.3.0) (2026-11-26)


### ⚠ BREAKING CHANGES

* remove Gemini CLI support; Antigravity is the Google host ([#264](https://github.com/Langspan/LangSpan.dev/issues/264))

### Features

* a coordination policy for each kind of refusal, with an opt-in wait for a current holder (B28) ([#232](https://github.com/Langspan/LangSpan.dev/issues/232)) ([f8f1c1f](https://github.com/Langspan/LangSpan.dev/commit/f8f1c1fe92ed79518592542a19fd26a8a2a9832e))
* ask before a write what changed in its symbols (B26) ([#210](https://github.com/Langspan/LangSpan.dev/issues/210)) ([820e660](https://github.com/Langspan/LangSpan.dev/commit/820e660a2c407bae23d9d6aeeb0d4a29eac82a34))
* brief only what changed since a session was last told ([#222](https://github.com/Langspan/LangSpan.dev/issues/222)) ([#242](https://github.com/Langspan/LangSpan.dev/issues/242)) ([0232b1a](https://github.com/Langspan/LangSpan.dev/commit/0232b1abd1402c2bfc1d71fce03d10333f6ae708))
* commit --mine evidence for Codex, Cursor and Gemini CLI, and a status --mine pointer (B7) ([#225](https://github.com/Langspan/LangSpan.dev/issues/225)) ([3c15b14](https://github.com/Langspan/LangSpan.dev/commit/3c15b14d0853313be8f425fbb0241fb784cc00d9))
* hold broad commits on every host's shell and read only the taken paths' journal (B6) ([#233](https://github.com/Langspan/LangSpan.dev/issues/233)) ([458bd58](https://github.com/Langspan/LangSpan.dev/commit/458bd58987280a592c0a64f5c9f77820f81917c4))
* index C# source files (D6, second language) ([#240](https://github.com/Langspan/LangSpan.dev/issues/240)) ([6863694](https://github.com/Langspan/LangSpan.dev/commit/686369473032037942e1cf188623ca2b1c14c4d6))
* leave a trail when a session ends and tell the next session once (C9) ([#236](https://github.com/Langspan/LangSpan.dev/issues/236)) ([d17c1a6](https://github.com/Langspan/LangSpan.dev/commit/d17c1a6d950bafa5f797df139ff999426050eb53))
* measure the K1 benchmark's cost to a correct result with one repair round ([#224](https://github.com/Langspan/LangSpan.dev/issues/224)) ([82779d7](https://github.com/Langspan/LangSpan.dev/commit/82779d7784b0395652359674795364f5cdb4cde7))
* name definitions a clean combination breaks, on request (D9) ([#258](https://github.com/Langspan/LangSpan.dev/issues/258)) ([54fa757](https://github.com/Langspan/LangSpan.dev/commit/54fa757d28bb499dc36b461d599a5f2ae7ed822b))
* paths between symbols and a repository map (D3) ([#252](https://github.com/Langspan/LangSpan.dev/issues/252)) ([2c510dc](https://github.com/Langspan/LangSpan.dev/commit/2c510dc41944e208349c1dfe0f97c5e28ff3af86))
* protect a peer's recent lines from unread shell and whole-file writes (B4) ([#271](https://github.com/Langspan/LangSpan.dev/issues/271)) ([0e52ec0](https://github.com/Langspan/LangSpan.dev/commit/0e52ec031ec2a979944782e38652a3ede1f3c41c))
* rank the code briefing and name each definition's dependents (C2) ([#227](https://github.com/Langspan/LangSpan.dev/issues/227)) ([23cac44](https://github.com/Langspan/LangSpan.dev/commit/23cac44ebe44d97b92e7f227674190ffc54e1b12))
* remove Gemini CLI support; Antigravity is the Google host ([#264](https://github.com/Langspan/LangSpan.dev/issues/264)) ([7af8526](https://github.com/Langspan/LangSpan.dev/commit/7af852643bdb1627fac1fc41c07ab253e5140278)), closes [#246](https://github.com/Langspan/LangSpan.dev/issues/246) [#195](https://github.com/Langspan/LangSpan.dev/issues/195)
* report a coordinator and hook binary of different versions in doctor (I5) ([#248](https://github.com/Langspan/LangSpan.dev/issues/248)) ([46fc3ad](https://github.com/Langspan/LangSpan.dev/commit/46fc3adf091459f880c6f19eeecb8d85378781f3))
* report identifier renames, adapt them on request, and rebase landings (F5) ([#254](https://github.com/Langspan/LangSpan.dev/issues/254)) ([d3f2b7c](https://github.com/Langspan/LangSpan.dev/commit/d3f2b7ccf81f5687fde6b4e4d58122d8b805390f))
* show what a session did after each refusal in the report (H9) ([#237](https://github.com/Langspan/LangSpan.dev/issues/237)) ([a9b6f88](https://github.com/Langspan/LangSpan.dev/commit/a9b6f880bc6fc3a656dfb4a876e27d303fcada09))
* tell interface notices between a Claude Code session's steps ([#214](https://github.com/Langspan/LangSpan.dev/issues/214)) ([#260](https://github.com/Langspan/LangSpan.dev/issues/260)) ([bf38316](https://github.com/Langspan/LangSpan.dev/commit/bf3831665f95ae709f370cf72c376fa76e5c2f2a))
* tell other worktrees about committed but unmerged interface changes ([#220](https://github.com/Langspan/LangSpan.dev/issues/220)) ([000360e](https://github.com/Langspan/LangSpan.dev/commit/000360e71c407a7466c1719e1d1b5b867d6ffa3b))
* track renames so history and notices follow the new name (D7) ([#203](https://github.com/Langspan/LangSpan.dev/issues/203)) ([89af39c](https://github.com/Langspan/LangSpan.dev/commit/89af39cf9d453818aeb50603fb35753a213d8178))


### Bug fixes

* make the K1 discovered_change interface change unavoidable and report change_not_made ([#219](https://github.com/Langspan/LangSpan.dev/issues/219)) ([6572357](https://github.com/Langspan/LangSpan.dev/commit/657235735846bfcb79da3d1229f1fdc29416900b)), closes [#218](https://github.com/Langspan/LangSpan.dev/issues/218)
* stop test coordinators on every exit path, and when their state folder goes ([#251](https://github.com/Langspan/LangSpan.dev/issues/251)) ([a65ba50](https://github.com/Langspan/LangSpan.dev/commit/a65ba50c7a5287a9d8cc9d576631acf8760469f8))

## [0.2.0](https://github.com/Langspan/LangSpan.dev/compare/v0.1.0...v0.2.0) (2026-11-26)


### Features

* a coordination report per session and host (H8) ([#183](https://github.com/Langspan/LangSpan.dev/issues/183)) ([efa5f8b](https://github.com/Langspan/LangSpan.dev/commit/efa5f8b753a5485e22de6a6386421d66325b5fd2))
* a K1 family whose interface change no task text names, a staggered start, and no auto memory ([#215](https://github.com/Langspan/LangSpan.dev/issues/215)) ([da308e2](https://github.com/Langspan/LangSpan.dev/commit/da308e2c2e165a2490b3b8beb54576b972588f47))
* a LangSpan block for AGENTS.md and rules files (I7) ([#155](https://github.com/Langspan/LangSpan.dev/issues/155)) ([6723e30](https://github.com/Langspan/LangSpan.dev/commit/6723e300557e03dae957c29e2ed55471d507df01))
* a monthly refusal count, langspan count (H5) ([#151](https://github.com/Langspan/LangSpan.dev/issues/151)) ([c59310a](https://github.com/Langspan/LangSpan.dev/commit/c59310aff251f7e8149005274ad1394acee58638))
* a shorter MCP tool list on hooked hosts (I11) ([#182](https://github.com/Langspan/LangSpan.dev/issues/182)) ([980f4c4](https://github.com/Langspan/LangSpan.dev/commit/980f4c4d3066532f8275e8048ce0f485fc096e6d))
* add the K1 coordination benchmark driver, fixture and oracle ([#184](https://github.com/Langspan/LangSpan.dev/issues/184)) ([4b39f89](https://github.com/Langspan/LangSpan.dev/commit/4b39f89707b1a71e4e751f076e3fdbdec94c185b))
* alert writes whose calls no longer fit a peer's signature, and follow renames (B9, B10) ([#179](https://github.com/Langspan/LangSpan.dev/issues/179)) ([4eb775b](https://github.com/Langspan/LangSpan.dev/commit/4eb775b21c2fa390c87f00063707a2dc3d83cc33))
* an Antigravity host adapter (A18) ([#140](https://github.com/Langspan/LangSpan.dev/issues/140)) ([d4f1887](https://github.com/Langspan/LangSpan.dev/commit/d4f188711ac9527d2d822adf77f4a27e87e88dcd))
* ask who received a message (E12) ([#186](https://github.com/Langspan/LangSpan.dev/issues/186)) ([43a35f4](https://github.com/Langspan/LangSpan.dev/commit/43a35f479640766d4a96c84d53438f21aa0ef621)), closes [#67](https://github.com/Langspan/LangSpan.dev/issues/67)
* desktop notices for refusals, failed checks and waiting handoffs ([#145](https://github.com/Langspan/LangSpan.dev/issues/145)) ([fba8ff0](https://github.com/Langspan/LangSpan.dev/commit/fba8ff0673fe2efc4c72eb1041d00a809eb7fb29))
* export and import the source index (D5) ([#185](https://github.com/Langspan/LangSpan.dev/issues/185)) ([8e0b37f](https://github.com/Langspan/LangSpan.dev/commit/8e0b37f861709e415df8eafbb066ec6f35b8b423))
* index Java source files (D6, first language) ([#189](https://github.com/Langspan/LangSpan.dev/issues/189)) ([af098f0](https://github.com/Langspan/LangSpan.dev/commit/af098f0b2c644497dbb04451e4851539512cfa1d))
* keep sessions current from recent hook runs, and probe processes on macOS (J12) ([#160](https://github.com/Langspan/LangSpan.dev/issues/160)) ([215b201](https://github.com/Langspan/LangSpan.dev/commit/215b201da3616a659546ad8b7fc296129fa639a5))
* learn which tests flip on unchanged code (F2) ([#125](https://github.com/Langspan/LangSpan.dev/issues/125)) ([fd9391b](https://github.com/Langspan/LangSpan.dev/commit/fd9391bd3e9acd3d47a1d1c10aa7d04b7e2d3fa9))
* let a session adopt an ended session's uncommitted work ([#176](https://github.com/Langspan/LangSpan.dev/issues/176)) ([22e9735](https://github.com/Langspan/LangSpan.dev/commit/22e97353c2deb4a3138ac283072d4b961742438e))
* move, switch and remove a workspace (G6) ([#134](https://github.com/Langspan/LangSpan.dev/issues/134)) ([c943cb2](https://github.com/Langspan/LangSpan.dev/commit/c943cb245ba516d787ca6ee5bb98d8945806129a))
* rebuild the dashboard as LangSpan Live ([#199](https://github.com/Langspan/LangSpan.dev/issues/199)) ([b264b7d](https://github.com/Langspan/LangSpan.dev/commit/b264b7dc01679622f7b6cd8e6484bcf4e16a670d))
* retention limits for the event log (J7) ([#117](https://github.com/Langspan/LangSpan.dev/issues/117)) ([f48b57b](https://github.com/Langspan/LangSpan.dev/commit/f48b57b5aece4d4eaf169d21ef9f6e6cbdb364d9))
* run the wider check when an interface changes (F3) ([#190](https://github.com/Langspan/LangSpan.dev/issues/190)) ([d354bc1](https://github.com/Langspan/LangSpan.dev/commit/d354bc191e7418a9d0ef71cc38d9692957b7d4ec))
* say which tests a write broke (F1) ([#192](https://github.com/Langspan/LangSpan.dev/issues/192)) ([2090d68](https://github.com/Langspan/LangSpan.dev/commit/2090d68df1f7d76553092b4e4931b7571c36bf68))
* set up in one command and keep the coordinator running ([#178](https://github.com/Langspan/LangSpan.dev/issues/178)) ([ef94746](https://github.com/Langspan/LangSpan.dev/commit/ef947469829e2441762f878e301067a2fe98367f))
* show a message a briefing cut, in full (E13) ([#120](https://github.com/Langspan/LangSpan.dev/issues/120)) ([dc17e52](https://github.com/Langspan/LangSpan.dev/commit/dc17e525fbfe78ee45bfc4aa9fce1e0e833fb553))
* show peers where a session is looking before it writes (A13) ([#156](https://github.com/Langspan/LangSpan.dev/issues/156)) ([822095d](https://github.com/Langspan/LangSpan.dev/commit/822095dd8fee39bd72596683a29f803dccc73bd1))
* show the history of a definition with langspan graph history (D4) ([#157](https://github.com/Langspan/LangSpan.dev/issues/157)) ([b0b7fdd](https://github.com/Langspan/LangSpan.dev/commit/b0b7fdd204d4023f25b1222620bd27ba1ab7aab5))
* tell a new session what changed recently in the repository (C3) ([#181](https://github.com/Langspan/LangSpan.dev/issues/181)) ([3702059](https://github.com/Langspan/LangSpan.dev/commit/3702059919ddd15dc59cc295678eda08d922c0a1))
* tell dependents about another worktree's uncommitted signatures (C7) ([#180](https://github.com/Langspan/LangSpan.dev/issues/180)) ([f8ecd24](https://github.com/Langspan/LangSpan.dev/commit/f8ecd24ee0301d1a7025a12b5ffea3204993b828))
* withdraw a sent message and tell its readers to disregard it ([#150](https://github.com/Langspan/LangSpan.dev/issues/150)) ([3d2cfb4](https://github.com/Langspan/LangSpan.dev/commit/3d2cfb4a78750b7ede8c156174342ee6cdad7d9c)), closes [#66](https://github.com/Langspan/LangSpan.dev/issues/66)


### Bug fixes

* bring the skill back under the host size limit ([#204](https://github.com/Langspan/LangSpan.dev/issues/204)) ([ab59259](https://github.com/Langspan/LangSpan.dev/commit/ab59259e43a658ed38ff403ec52b5a84d2ade323))
* coordinate Antigravity 1.5.14's real tools and answers ([#191](https://github.com/Langspan/LangSpan.dev/issues/191)) ([dc134bf](https://github.com/Langspan/LangSpan.dev/commit/dc134bf8ec2f8a48da3fe8681eadf7243e2b7da1))
* create the daemon socket owner-only before clients can see it ([#206](https://github.com/Langspan/LangSpan.dev/issues/206)) ([230c7b4](https://github.com/Langspan/LangSpan.dev/commit/230c7b403d5bbea8e5f1c0a08bd18c7b5c6b265f)), closes [#205](https://github.com/Langspan/LangSpan.dev/issues/205)
* don't tell a session its checkout changed when its own Git command changed it ([#149](https://github.com/Langspan/LangSpan.dev/issues/149)) ([d92b92b](https://github.com/Langspan/LangSpan.dev/commit/d92b92b73ce2aecb2d502f11221f57107628d1b7))
* indentation and string spacing are content, not formatting; tell sessions when files changed with no hook reported ([#162](https://github.com/Langspan/LangSpan.dev/issues/162)) ([0c57514](https://github.com/Langspan/LangSpan.dev/commit/0c57514d767cad5f8c54f44875e1664a6f9bf1de))
* isolate K1 benchmark sessions, sum every result and make hedges fail ([#211](https://github.com/Langspan/LangSpan.dev/issues/211)) ([fba5277](https://github.com/Langspan/LangSpan.dev/commit/fba5277f13546a5f926d851cc9324166c35df314))
* main's Go output classification and the benchmark dry run ([#216](https://github.com/Langspan/LangSpan.dev/issues/216)) ([13e0396](https://github.com/Langspan/LangSpan.dev/commit/13e03962f00bab14c7780560bdd8f708903fff57))
* make the K1 harness arms distinguishable ([#193](https://github.com/Langspan/LangSpan.dev/issues/193), [#188](https://github.com/Langspan/LangSpan.dev/issues/188)) ([#197](https://github.com/Langspan/LangSpan.dev/issues/197)) ([d6c9912](https://github.com/Langspan/LangSpan.dev/commit/d6c9912e50079b44477f138497448cf41bb3dce4))
* make the scan-bounds and following-briefings tests independent of machine speed ([#200](https://github.com/Langspan/LangSpan.dev/issues/200)) ([#202](https://github.com/Langspan/LangSpan.dev/issues/202)) ([a8a566d](https://github.com/Langspan/LangSpan.dev/commit/a8a566d43b2ef2095e2cd1cae274a9f068ae8e57))
* tell a peer in another worktree of a rename at the write that calls it ([#209](https://github.com/Langspan/LangSpan.dev/issues/209)) ([0518671](https://github.com/Langspan/LangSpan.dev/commit/05186715a74d8313e1b8759a9ecb6ecd6acb0c63))
* treat a checkout reached through a symbolic link as the same checkout ([#174](https://github.com/Langspan/LangSpan.dev/issues/174)) ([a7d58bb](https://github.com/Langspan/LangSpan.dev/commit/a7d58bbb598819f35e0fdf4dc36b7dada84cfe1e))

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
