# SuperDuper DSP — ТЗ v0.1

> ⚠️ **ARCHIVED — superseded design.**
>
> This document describes the original "shell plugin with hot-reloaded
> dylibs + daemon + MCP server" architecture (commits prior to mid-2026).
> That design was abandoned because REAPER caches param layouts per
> `(plugin_id, FX-slot)` in the project file, which made dynamic
> parameter tables unworkable.
>
> The shipping architecture is **one standalone CLAP plugin per effect**
> (one fixed param table, one stable CLAP id). 13 of those live under
> `effects/superduper-*/`. See [`README.md`](README.md) for the current
> picture and [`CLAUDE.md`](CLAUDE.md) for the per-codebase walkthrough.
>
> Kept here for historical context only — DO NOT use as guidance for
> current work.

## Что это

Headless CLAP-плагин для любого DAW (REAPER, Bitwig, FL Studio) с внешним MCP-сервером. Управление через Claude Code в терминале: Claude пишет Rust DSP-код, плагин компилирует его в native .dylib и горячо подгружает в audio thread.

Без GUI code editor'а, без AI-генерации внутри плагина — вся работа происходит в Claude Code, плагин только исполняет.

## Концепция

```
┌──────────────┐    HTTP/SSE     ┌───────────────────────┐
│ Claude Code  │ ◄────MCP────►   │ superduperd (daemon)  │
│ (терминал)   │  :7891          │ singleton process     │
└──────────────┘                 └────────┬──────────────┘
                                          │ Unix socket
                                          │ /tmp/superduper-dsp.sock
                ┌─────────────────────────┴────────────────┐
                │                                          │
        ┌───────▼──────────┐                    ┌──────────▼──────────┐
        │ SuperDuperDSP    │                    │ SuperDuperDSP       │
        │ .clap #1 "Lead"  │                    │ .clap #2 "Drums"    │
        │                  │                    │                     │
        │ ┌──────────────┐ │                    │ ┌─────────────────┐ │
        │ │ active.dylib │ │                    │ │ active.dylib    │ │
        │ │ (hot-loaded) │ │                    │ │ (hot-loaded)    │ │
        │ └──────────────┘ │                    │ └─────────────────┘ │
        └──────────────────┘                    └─────────────────────┘
        (track 1 in DAW)                        (track 2 in DAW)
```

Один daemon обслуживает все инстансы плагина в DAW. Claude Code подключается к одному MCP-серверу, видит все live инстансы как targets, может управлять любым по имени трека или UUID.

## Use cases

**UC-1.** Загрузил SuperDuper DSP на трек. В терминале:
```
$ claude
> сделай tape saturation с тёплыми низами на треке Lead
Claude: [генерирует Rust код]
Claude: [list_instances → находит "Lead"]
Claude: [load_effect(target="Lead", code=<rust>)]
Plugin: компилит → грузит → играет
```

**UC-2.** Live tuning параметров:
```
> убавь drive
Claude: [set_param(target="Lead", param="drive", value=0.3)]
```

**UC-3.** Multi-track оркестрация:
```
> на Drums transient shaper, на Bass — saturation
Claude: [параллельно работает с двумя инстансами через единый MCP]
```

**UC-4.** Сохранение пресетов:
```
> сохрани цепочку как "warm_mix"
Claude: [save_session(name="warm_mix")]
```

## Функциональные требования

### F1 — Plugin lifecycle
- При первой загрузке плагина в DAW → форкает daemon (если ещё не запущен)
- Регистрируется в daemon через Unix socket: UUID + optional name + track_name (если CLAP host extension доступен)
- Heartbeat каждые 5 сек
- При выгрузке — clean unregister
- Когда последний инстанс выгрузился — daemon ждёт 30 сек и завершается

### F2 — Hot-reload native DSP code
- MCP tool `load_effect(target, code)` принимает Rust-код
- Daemon записывает код в `~/.superduper-dsp/instances/<uuid>/src/process.rs`
- Запускает `cargo build --release --crate-type cdylib` в этой папке
- При успехе → отсылает плагину через socket `LoadDylib { path }`
- Плагин делает `dlopen` + `dlsym("process")` + atomic ptr swap (audio thread безопасно подхватывает на следующем callback)
- Старая .dylib держится ~50ms (для угасания текущих voice'ов), потом `dlclose`
- При ошибке компиляции → возвращает `cargo` stderr в Claude через MCP

### F3 — Параметры
- DSP-код декларирует параметры через макрос `params!` из `superduper-dsp-sdk`
- Макрос генерит:
  - Константы-индексы (`pub const DRIVE: usize = 0`)
  - Stable ABI export `get_param_count() -> u32`
  - Stable ABI export `get_param_metadata(idx) -> ParamMeta`
- Daemon после компиляции читает метаданные через `libloading`
- MCP `get_params(target)` возвращает список с min/max/default/current/unit
- MCP `set_param(target, name, value)` пишет в atomic float, audio thread читает atomically каждый callback
- CLAP host получает параметры через `params.rescan()` → ползунки появляются в DAW для автоматизации

### F4 — MCP tools API

| Tool | Args | Returns |
|---|---|---|
| `list_instances` | — | `[{id, name, track_name, current_effect, status}]` |
| `load_effect` | `target, code` | `{success, compile_log, params?, error?}` |
| `get_params` | `target` | `[{name, min, max, default, current, unit}]` |
| `set_param` | `target, name, value` | `{success, error?}` |
| `get_code` | `target` | `{code: String}` (для inspect / "что сейчас крутится") |
| `bypass` | `target, enabled` | `{success}` |
| `rename_instance` | `target, name` | `{success}` |
| `save_session` | `name` | `{success, path}` |
| `load_session` | `name` | `{success, instances_restored}` |
| `get_status` | `target` | `{cpu_usage, samples_processed, error_log}` |

**Target** = UUID / instance name / track name (daemon резолвит).

### F5 — Plugin GUI (минимальный)
- При FX → Edit открывается окно 400×300
- Содержит:
  - Instance name input
  - Status: "Idle / Compiling / Running / Error"
  - Compile log (read-only)
  - Список текущих параметров с values (read-only в v0.1)
  - Bypass toggle
- Никакого code editor'а — это by design

### F6 — superduper-dsp-sdk
- Rust crate, который пользователь подключает в `process.rs`
- Экспонирует:
  - `params! { ... }` — декларация параметров
  - `setup!()` — инициализация ABI exports
  - DSP-утилиты: envelope follower, biquad filters, oversampling helpers, denormal guard
  - Опционально: совместимый ABI с ConjureDSP (чтобы импортировать их Rust-пресеты)

### F7 — Сохранение пресетов
- Session = снимок всех живых инстансов: код + параметры + имена
- Хранится в `~/.superduper-dsp/sessions/<name>.toml`
- `load_session` ищет соответствие по имени инстанса/трека, восстанавливает

### F8 — Совместимость с total-reaper-mcp
- Никаких прямых интеграций в v0.1 — оба MCP-сервера живут параллельно
- Claude Code видит оба и сам оркестрирует
- В v0.2 опционально: bridge MCP-tool который вставляет SuperDuperDSP в трек через total-reaper-mcp

## Нефункциональные требования

### NF1 — Real-time safety
- Audio thread:
  - Никогда не аллоцирует
  - Никогда не блокируется на mutex/syscall
  - Только atomic-чтения указателя `process` и параметров
  - FTZ/DAZ установлены в audio thread
- `cargo build` происходит **в daemon процессе**, не в плагине
- `dlopen` новой .dylib делается в **non-RT thread плагина**, swap — атомарный

### NF2 — Производительность
- Latency между "Claude wrote code" и "звук изменился": ≤ 3 сек (зависит от cargo incremental)
- CPU overhead плагина без активного эффекта (pass-through): < 0.5%
- Память: ≤ 50MB на инстанс

### NF3 — Безопасность
- DSP-код исполняется в audio thread процесса DAW → полный доступ к памяти
- **Trust model**: код от Claude, пользователь видит код перед компиляцией (в GUI или в Claude Code prompt'е), ответственность на пользователе
- В v0.1: pre-load log показывает diff кода перед компиляцией
- В v0.2: optional wasmtime sandbox для untrusted кода

## Стек

| Компонент | Технология |
|---|---|
| Plugin language | Rust 1.78+ |
| Plugin API | `clack-plugin` (idiomatic Rust CLAP bindings) |
| Daemon | Rust + tokio + axum (HTTP SSE for MCP) |
| Plugin↔Daemon IPC | Unix domain socket, `interprocess` crate, JSON-lines |
| Hot-reload | `libloading` для dlopen/dlsym + `cargo` subprocess |
| Audio buffer | `ringbuf` crate (SPSC lock-free) |
| MCP server | Custom impl over axum (simpler than full SDK for our scope) |
| Plugin GUI | `nih_plug_egui` или raw Cocoa через `objc2` |
| Build orchestration | Cargo workspace |

## Структура проекта

```
superduper-dsp/
├─ Cargo.toml                    # workspace root
├─ README.md
├─ SPEC.md                       # этот файл
├─ CLAUDE.md                     # инструкции для Claude Code
├─ LICENSE
├─ scripts/
│  ├─ build_bundle.sh            # собирает .clap бандл для macOS
│  ├─ install_local.sh           # копирует в ~/Library/Audio/Plug-Ins/CLAP/
│  └─ dev_loop.sh                # watch + auto-reload во время разработки
├─ plugin/
│  ├─ Cargo.toml
│  └─ src/
│     ├─ lib.rs                  # CLAP plugin entry
│     ├─ host.rs                 # Plugin lifecycle
│     ├─ process.rs              # Audio callback (real-time safe)
│     ├─ daemon_client.rs        # Unix socket client
│     ├─ hotreload.rs            # dlopen + atomic ptr swap
│     ├─ gui.rs                  # Minimal egui GUI
│     └─ params.rs               # CLAP params bridge
├─ daemon/
│  ├─ Cargo.toml
│  └─ src/
│     ├─ main.rs                 # Entry, lifecycle
│     ├─ mcp.rs                  # MCP server (axum SSE)
│     ├─ registry.rs             # Instance registry
│     ├─ ipc.rs                  # Unix socket server
│     ├─ build_pipeline.rs       # cargo subprocess invocation
│     ├─ dylib_inspector.rs      # Read param metadata from .dylib
│     └─ sessions.rs             # Save/load sessions
├─ protocol/
│  ├─ Cargo.toml
│  └─ src/lib.rs                 # Shared types: IPC + MCP messages
├─ sdk/
│  ├─ Cargo.toml
│  └─ src/
│     ├─ lib.rs                  # params! macro, setup!, ABI
│     └─ dsp/                    # Filters, envelopes, helpers
└─ effects/
   └─ example-passthrough/
      ├─ Cargo.toml
      └─ src/process.rs          # Reference example
```

## Roadmap

### M1 — Hello CLAP + Daemon handshake (3 вечера)
- [ ] Cargo workspace собирается чисто
- [ ] `scripts/build_bundle.sh` создаёт `SuperDuperDSP.clap` бандл
- [ ] Плагин виден в REAPER, не крашится
- [ ] При первой загрузке: daemon форкается и регистрирует инстанс
- [ ] При выгрузке: clean unregister
- [ ] MCP server отвечает на `list_instances`

### M2 — Full MCP server (2 вечера)
- [ ] Claude Code подключается через `http://127.0.0.1:7891/sse`
- [ ] `list_instances`, `bypass`, `rename_instance` работают
- [ ] `load_effect` принимает код и сохраняет (stub success)
- [ ] `set_param` передаёт значение через socket (без эффекта пока)

### M3 — Hot-reload Rust code (3 вечера) ⚠️ риск
- [ ] Daemon вызывает `cargo build --release --crate-type cdylib`
- [ ] При успехе: путь к .dylib передаётся плагину
- [ ] Плагин делает dlopen + atomic swap
- [ ] Audio thread на следующем callback вызывает новый `process()`
- [ ] При ошибке компиляции — stderr → MCP response
- [ ] Старая .dylib корректно закрывается через ~50ms

### M4 — Params system (2 вечера)
- [ ] `params!` макрос в SDK генерит ABI exports
- [ ] Daemon читает метаданные параметров через libloading
- [ ] `get_params` возвращает корректный список
- [ ] `set_param` меняет atomic float, audio thread читает
- [ ] CLAP host видит параметры (через rescan)

### M5 — Minimal GUI (1 вечер)
- [ ] При FX → Edit открывается окно 400×300
- [ ] Instance name editable
- [ ] Status + compile log виден
- [ ] Список параметров с current values
- [ ] Bypass toggle работает

### M6 — Sessions (1 вечер)
- [ ] `save_session(name)` создаёт TOML со всеми инстансами
- [ ] `load_session(name)` восстанавливает
- [ ] Graceful handling: если инстанс отсутствует — warning, не ошибка

### M7 — Polish + release (1 вечер)
- [ ] CLAUDE.md инструкции
- [ ] README со скриншотами
- [ ] Examples: passthrough, gain, soft-clip, simple delay
- [ ] GitHub release с .clap бандлом

**Итого: 13 вечеров до v0.1**

## Out of scope (v0.1)

- Audio snapshot для Claude feedback (v0.2)
- Spectrum analysis API (v0.2)
- Wasmtime sandbox (v0.2)
- GUI sliders для параметров (v0.2)
- Windows/Linux builds (v0.3)
- VST3/AU обёртки (всегда CLAP-only)
- Code editor в GUI (никогда — by design)

## Distribution model (decided 2026-05): A+C hybrid

End goal: AI-generated effects ship as real plugins users can drop into REAPER
without Claude or cargo. Two-stage rollout:

**Stage A — shell + dropdown (M2–M3)**
- One `SuperDuper DSP.clap` plugin acts as a container.
- Effects live as standalone `.dylib` files in
  `~/Library/Audio/Plug-Ins/SuperDuper Effects/<name>.dylib` (one per effect,
  optionally accompanied by `<name>.json` for params metadata).
- The shell plugin scans that folder on activate, exposes the list via a CLAP
  enum/stepped parameter ("Effect: ▼ Tape Saturator / Sub Reverb / …"). Changing
  the param triggers `slot.swap()` to the chosen dylib.
- Sharing an effect = sending a `.dylib` file; recipient drops it into the
  folder, dropdown updates next time the plugin scans.
- The existing per-instance `~/.superduper-dsp/instances/<uuid>/effect.dylib`
  remains as a development sandbox for MCP `load_effect` iterations.

**Stage C — freeze to standalone `.clap` (M5+)**
- MCP tool `freeze(effect, name, vendor)` takes a working effect and produces
  `<name>.clap` — a self-contained CLAP plugin with the dylib embedded via
  `include_bytes!` and unique `clap_plugin_id`.
- Implemented as a Cargo template (`clap-shell-template/`) with placeholders
  for id / name / vendor / dylib bytes / params descriptor. Build pipeline:
  template → cargo build → bundle assembly.
- Each frozen plugin shows up in REAPER FX browser as a distinct entry
  (`SuperDuper: Tape Saturator`, etc.), no shell needed, no Claude needed.
- Sharing = sending one `.clap` file.

**What we are NOT doing:** B (multi-plugin factory in one .clap) — adds factory
complexity to the runtime shell without distribution wins over C.

## Открытые вопросы

1. **Conduit-SDK ABI совместимость с ConjureDSP** — взять их sig или сделать свой?
2. **Дефолтный effect template** — какой код встроить?
3. **Параметры через CLAP rescan на каждый load_effect** — может ли это вызвать audio glitches в некоторых хостах?
4. **Effect dylib filename → display name mapping** — берём из `<name>.json` sidecar
   или экспортируем дополнительный ABI symbol `sdsp_effect_display_name()`?

## Связь с экосистемой SuperDuperAI

- **openai-oxide** — будущая интеграция в daemon для optional "auto-fix compile errors" (Claude API внутри daemon)
- **Akbuzat** — пресеты можно шарить через decentralized mesh
- **rust-code** — terminal agent на Gemini может управлять SuperDuper DSP параллельно с Claude Code
- **sgr-agent** — pattern typed dispatch для protocol messages

SuperDuper DSP — это **AI-first creative tool** для аудио, продолжение линейки AI video editor / FaceAlarm / Super Chatbot.
