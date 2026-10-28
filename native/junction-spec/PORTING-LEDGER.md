# Junction 移植台帳

`tools/junction-spec/baseline.mjs --write` が TESTMAP.json から生成する。手で編集しない。状態を変えるときは TESTMAP.json の `port.status`（と `port.rust`、`port.note`）を書き換えてから `--write` する。

テスト 198 件：kept 54、todo 144。

| 項目 | 層 | ファイル | スイート | 方針 | 件数 | 状態 |
|---|---|---|---|---|---|---|
| — | signaling | `services/junction-signaling/test/config.test.ts` |  | keep | 1 | kept 1 |
| — | signaling | `services/junction-signaling/test/protocol.test.ts` |  | keep | 13 | kept 13 |
| — | signaling | `services/junction-signaling/test/rate-limit.test.ts` |  | keep | 8 | kept 8 |
| — | signaling | `services/junction-signaling/test/rooms.test.ts` |  | keep | 19 | kept 19 |
| — | signaling | `services/junction-signaling/test/server.test.ts` |  | keep | 13 | kept 13 |
| S12 | bridge | `src-tauri/src/junction_mcp_bridge.rs` |  | port | 4 | todo 4 |
| S12 | bridge | `src-tauri/src/share_musics.rs` |  | port | 1 | todo 1 |
| W2-33 | core | `native/mixxx-engine-host/tests/junction/authority_test.cpp` | `authority` | port | 2 | todo 2 |
| W2-33 | core | `native/mixxx-engine-host/tests/junction/manual_exchange_test.cpp` | `manual-exchange` | port | 3 | todo 3 |
| W2-33 | core | `native/mixxx-engine-host/tests/junction/media_transport_test.cpp` | `ice-servers` | port | 1 | todo 1 |
| W2-33 | core | `native/mixxx-engine-host/tests/junction/network_settings_test.cpp` | `network-settings` | port | 4 | todo 4 |
| W2-33 | core | `native/mixxx-engine-host/tests/junction/program_mixer_test.cpp` | `program_mixer` | port | 6 | todo 6 |
| W2-33 | core | `native/mixxx-engine-host/tests/junction/turn_state_test.cpp` | `turn` | port | 9 | todo 9 |
| W2-34 | core | `native/mixxx-engine-host/tests/junction/clock_test.cpp` | `asrc` | port | 2 | todo 2 |
| W2-34 | core | `native/mixxx-engine-host/tests/junction/clock_test.cpp` | `audio-clock-anchor` | port | 1 | todo 1 |
| W2-34 | core | `native/mixxx-engine-host/tests/junction/clock_test.cpp` | `clock-estimator` | port | 3 | todo 3 |
| W2-34 | core | `native/mixxx-engine-host/tests/junction/clock_test.cpp` | `media-timeline` | port | 3 | todo 3 |
| W2-34 | core | `native/mixxx-engine-host/tests/junction/clock_test.cpp` | `pcm-ring` | port | 8 | todo 8 |
| W2-34 | core | `native/mixxx-engine-host/tests/junction/junction_input_test.cpp` | `junction input` | port | 1 | todo 1 |
| W2-34 | core | `native/mixxx-engine-host/tests/junction/junction_input_test.cpp` | `junction-input` | port | 16 | todo 16 |
| W2-34 | core | `native/mixxx-engine-host/tests/junction/producer_tap_test.cpp` | `producer-tap` | port | 1 | todo 1 |
| W2-38 | transport | `native/mixxx-engine-host/tests/junction/media_transport_test.cpp` | `media-relay` | port | 1 | todo 1 |
| W2-38 | transport | `native/mixxx-engine-host/tests/junction/media_transport_test.cpp` | `media-transport` | port | 4 | todo 4 |
| W2-42 | runtime | `native/mixxx-engine-host/tests/junction/manual-runtime.test.mjs` |  | port | 17 | todo 17 |
| W2-42 | runtime | `native/mixxx-engine-host/tests/junction/runtime.test.mjs` |  | port | 1 | todo 1 |
| W2-44 | runtime | `native/mixxx-engine-host/tests/junction/program_output_test.cpp` | `program-output` | port | 1 | todo 1 |
| W2-44 | runtime | `native/mixxx-engine-host/tests/junction/program_output_test.cpp` | `program-output-device` | port | 1 | todo 1 |
| W2-44 | runtime | `native/mixxx-engine-host/tests/junction/program_output_test.cpp` | `program-output-long-device` | port | 1 | todo 1 |
| W2-44 | runtime | `native/mixxx-engine-host/tests/junction/program_output_test.cpp` | `program-output-splice-device` | port | 1 | todo 1 |
| W2-45 | interop | `native/mixxx-engine-host/tests/junction/cross-platform-runtime.test.mjs` |  | port | 2 | todo 2 |
| W2-47 | frontend | `src/services/junction/control-boundary.test.mjs` |  | rerun | 5 | todo 5 |
| W2-47 | frontend | `src/services/junction/exchange-actions.test.mjs` |  | rerun | 9 | todo 9 |
| W2-47 | frontend | `src/services/junction/program-mixer.test.mjs` |  | rerun | 3 | todo 3 |
| W2-47 | frontend | `src/services/junction/roster-model.test.mjs` |  | rerun | 12 | todo 12 |
| W2-47 | frontend | `src/services/junction/share-musics/automation.test.mjs` |  | rerun | 10 | todo 10 |
| W2-47 | frontend | `src/services/junction/turn-state.test.mjs` |  | rerun | 6 | todo 6 |
| W2-47 | frontend | `src/services/midi/junction-channel.test.mjs` |  | rerun | 4 | todo 4 |
| W2-47 | frontend | `src/services/update-controller.test.mjs` |  | rerun | 1 | todo 1 |

## op と実装の不一致

- C++ suites "junction input" and "junction-input" name the same module; a same-name port keeps both
- command private.stop: handled by the runtime but missing from JunctionOp
