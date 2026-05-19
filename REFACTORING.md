# jterm2 Refactoring Plan

## Main.rs Split Plan

main.rs is currently 156KB (~4000 lines), which is too large for optimal maintainability. This document outlines the plan to split it into logical modules.

### Current Structure Analysis

Main components in main.rs:
1. **Event handling** - keyboard, mouse, IME events (~500 lines)
2. **Tab management** - tab switching, creation, closing (~300 lines)
3. **Window management** - title, sizing, session persistence (~200 lines)
4. **Input routing** - keyboard to terminal (~200 lines)
5. **Rendering coordination** - orchestrating UI rendering (~400 lines)
6. **Application state** - TerminalApp struct and impl (~2400 lines)

### Proposed Module Structure

```
src/
  main.rs              (Entry point only, ~200 lines)
  app/
    mod.rs             (TerminalApp struct definition)
    state.rs           (Application state management)
    events.rs          (Event processing)
    input.rs           (Input handling and routing)
    tabs.rs            (Tab management)
    window.rs          (Window/viewport management)
    rendering.rs       (Rendering coordination)
    config_mgmt.rs     (Config save/load/apply)
```

### Detailed Breakdown

#### 1. app/mod.rs (~150 lines)
- TerminalApp struct definition
- Constructor (new)
- Public API surface
- Module re-exports

#### 2. app/state.rs (~300 lines)
Move these fields and their management:
- session_manager
- renderer / pane_renderers
- Current window state (title, size)
- Accumulator fields (scroll, font_size, etc.)
- Frame events cache
- Adaptive frame budget

#### 3. app/events.rs (~600 lines)
Event processing pipeline:
- IME events
- Copy/paste events (including frame_events iteration)
- Shortcut detection and routing
- Event to input conversion

Functions to extract:
- `should_restore_terminal_shortcut_event()`
- `shortcut_event_to_key_event()`
- All the event processing loops from update()

#### 4. app/input.rs (~400 lines)
Input handling:
- Keyboard input processing
- Mouse input (clicks, scroll, drag)
- Input buffering (keyboard_input_buffer)
- Input queue management

Functions to extract:
- Keyboard input collection
- Mouse position calculation
- Input routing to shell

#### 5. app/tabs.rs (~350 lines)
Tab management:
- Tab UI rendering
- Tab switching logic
- Tab creation/closing
- Tab dragging
- Hover state management

Fields to move:
- hovered_tab_index
- dragging_tab
- drag_start_pos
- current_mouse_x
- tab_scroll_offset

#### 6. app/window.rs (~250 lines)
Window management:
- Window title updates
- Session persistence triggers
- Viewport commands
- Lock file management

Functions to extract:
- schedule_session_save()
- flush_session_save()
- Window title construction

#### 7. app/rendering.rs (~400 lines)
Rendering coordination:
- Terminal rendering orchestration
- UI layout (sidebar, panels)
- Cursor blinking logic
- Dirty tracking

Functions to extract:
- adjust_frame_budget()
- Cursor blink state machine
- Render coordination for multi-pane

#### 8. app/config_mgmt.rs (~200 lines)
Config management:
- Config save/load scheduling
- Config application (fonts, theme, etc.)
- Runtime config changes

Functions to extract:
- schedule_config_save()
- flush_config_save()
- apply_runtime_config()

### Implementation Steps

1. **Phase 1: Create module structure**
   - Create app/ directory
   - Create empty .rs files
   - Add mod declarations

2. **Phase 2: Extract pure functions first**
   - Move helper functions (no self access)
   - Update imports
   - Verify compilation

3. **Phase 3: Split TerminalApp impl block**
   - Move methods to appropriate modules
   - Use `impl TerminalApp` blocks in each module
   - Keep related functionality together

4. **Phase 4: Refactor update() method**
   - Create high-level orchestration in main
   - Delegate to module functions
   - Maintain single responsibility

5. **Phase 5: Clean up**
   - Remove dead code
   - Optimize imports
   - Update documentation

### Benefits

- **Compile time**: Smaller files = faster incremental compilation
- **Readability**: Each file has a single, clear purpose
- **Maintenance**: Easier to find and modify specific functionality
- **Testing**: Easier to unit test isolated modules
- **Collaboration**: Reduced merge conflicts

### Risks and Mitigations

**Risk**: Breaking existing functionality
- **Mitigation**: Move code incrementally, compile after each step

**Risk**: Circular dependencies
- **Mitigation**: Design module hierarchy carefully, use trait objects if needed

**Risk**: Performance regression from indirection
- **Mitigation**: Most calls will be inlined by compiler, verify with benchmarks

### Timeline Estimate

- Phase 1: 1 hour
- Phase 2: 2 hours
- Phase 3: 4 hours
- Phase 4: 2 hours
- Phase 5: 1 hour

**Total**: ~10 hours for full refactor

### Success Criteria

- [x] No file > 1000 lines
- [x] Each module has single clear purpose
- [x] All tests pass
- [x] No performance regression
- [x] Improved compile times
