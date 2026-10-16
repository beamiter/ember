#!/usr/bin/env python3
"""Exercise extracted production IME admission/writer methods with small transport stubs.

This is not an OS IME or full GUI acceptance test. It executes the production
ownership reducer and native Enter tracker, and verifies old unguarded-writer
behavior fails with --check-regression. No repository source is rewritten.
"""
from pathlib import Path
import argparse
import os
import subprocess
import tempfile
root=Path(__file__).resolve().parents[1]
args=argparse.ArgumentParser(description=__doc__)
args.add_argument('--check-regression',action='store_true')
args=args.parse_args()
s=(root/'src/app/input.rs').read_text()
def method(src,name):
 start=src.index(name); brace=src.index('{',start); depth=1; end=brace+1
 while depth:
  if src[end]=='{':depth+=1
  if src[end]=='}':depth-=1
  end+=1
 return src[start:end]
methods=[]
for name in ['fn ime_batch_interaction_pos(', 'fn current_terminal_ime_owner(', 'pub(crate) fn cancel_terminal_ime(', 'fn sync_terminal_ime_owner(', 'pub(crate) fn prepare_terminal_ime_input(', 'pub(crate) fn discard_stale_frame_ime(', 'fn queue_ordered_ime_commit(']:
 methods.append(method(s,name).replace('crate::debug_log!', 'println!').replace('log::warn!', 'println!'))
h=method(s,'pub fn handle_ime_events(');h=h[:h.index('        let private_title')]+'has_preedit\n}';methods.append(h.replace('crate::debug_log!', 'println!'))
sw=method((root/'src/session_manager.rs').read_text(),'pub fn switch_session(')
fixture=r'''
#![allow(dead_code,unused_variables)]
#[path="ROOT/src/terminal_ime.rs"]mod terminal_ime;
use std::cell::{RefCell,RefMut};use std::sync::Arc;
extern crate self as winit;
pub mod keyboard {
#[derive(Clone,Copy,PartialEq,Eq,Hash)]pub enum KeyCode{Enter,NumpadEnter,KeyA}
#[derive(Clone,Copy,PartialEq,Eq,Hash)]pub enum PhysicalKey{Code(KeyCode)}
pub enum NamedKey{Enter}pub enum Key{Named(NamedKey)}
}
pub mod event {
pub struct State(pub bool);impl State{pub fn is_pressed(&self)->bool{self.0}}
pub struct KeyEvent{pub logical_key:crate::keyboard::Key,pub physical_key:crate::keyboard::PhysicalKey,pub state:State}
}
#[path="ROOT/src/native_enter.rs"]mod native_enter;

mod parking_lot {use super::*; pub struct Mutex<T>(pub RefCell<T>);impl<T> Mutex<T>{pub fn lock(&self)->RefMut<'_,T>{self.0.borrow_mut()}}}
mod egui {use super::*;pub type Pos2=super::Pos;
#[derive(Clone)]pub enum ImeEvent{Enabled,Preedit{text:String},Commit(String),DeleteSurrounding{},Disabled}
#[derive(Clone)]pub enum Key{Enter}
#[derive(Clone)]pub enum Event{Ime(ImeEvent),Key{key:Key,pressed:bool,repeat:bool},PointerButton{pressed:bool,pos:Pos2},PointerMoved(Pos2),PointerGone,MouseWheel{},WindowFocused(bool),Other}
pub struct RawInput{pub focused:bool,pub events:Vec<Event>}
pub struct Pointer{pub latest:Option<Pos2>}impl Pointer{pub fn latest_pos(&self)->Option<Pos2>{self.latest}}
pub struct Input {pub pointer:Pointer,pub focused:bool,pub events:Vec<Event>}
pub struct Context(pub RefCell<Input>);impl Default for Context{fn default()->Self{Self(RefCell::new(Input{pointer:Pointer{latest:None},focused:true,events:vec![]}))}}
impl Context{pub fn input<T>(&self,f:impl FnOnce(&Input)->T)->T{f(&self.0.borrow())}pub fn input_mut<T>(&self,f:impl FnOnce(&mut Input)->T)->T{f(&mut self.0.borrow_mut())}}
}
#[derive(Default)]struct Terminal{ime_enabled:bool,preedit_text:String}
impl Terminal{fn clear_preedit(&mut self){self.preedit_text.clear()}fn set_preedit(&mut self,t:String,_:usize){self.preedit_text=t}}
struct Metadata{session_id:String,unseen_output:bool}impl Metadata{fn update_last_active(&mut self){}}
struct Session{terminal:Arc<parking_lot::Mutex<Terminal>>,metadata:Metadata,bytes:Vec<u8>}
impl Session{fn queue_input(&mut self,b:&[u8])->bool{self.bytes.extend_from_slice(b);true}}
struct Manager{sessions:Vec<Session>,active_index:usize,previous_session_id:Option<String>}
impl Manager{fn sessions(&self)->&[Session]{&self.sessions}fn active_index(&self)->usize{self.active_index}fn get_active_session_mut(&mut self)->&mut Session{&mut self.sessions[self.active_index]} SWITCH}
struct Organism;impl Organism{fn accepted_input(&mut self,_:&str){}}
#[derive(Default)]struct Native{claims:usize,tracker:native_enter::NativeEnterOwnership}impl Native{fn claim_pressed(&mut self){self.claims+=1;self.tracker.claim_pressed();}
fn observe(&mut self,pressed:bool)->bool{self.tracker.observe(&event::KeyEvent{logical_key:keyboard::Key::Named(keyboard::NamedKey::Enter),physical_key:keyboard::PhysicalKey::Code(keyboard::KeyCode::Enter),state:event::State(pressed)},false)}}
#[derive(Clone,Copy)]struct Pos(f32);struct Rect(f32,f32);impl Rect{fn contains(&self,p:Pos)->bool{p.0>=self.0&&p.0<self.1}}
struct Pane{rect:Rect,session_idx:usize}struct Layout{panes:Vec<Pane>}impl Layout{fn panes(&self)->&[Pane]{&self.panes}fn divider_at(&self,_:Pos)->Option<()>{None}}
struct App{last_pointer_pos:Option<Pos>,layout:Layout,session_manager:Manager,frame_events:Vec<egui::Event>,organism:Organism,status_message:String,status_expires_at:Option<std::time::Instant>,active_session_epoch:u64,terminal_ime:terminal_ime::Ownership,frame_ime_owner:Option<terminal_ime::Owner>,ime_origin_terminal:Option<Arc<parking_lot::Mutex<Terminal>>>,native_enter_ownership:Native,blocked:bool}
impl App{fn layout(&self)->&Layout{&self.layout}fn clear_block_selection(&mut self){}fn terminal_input_blocked(&self,_:&egui::Context)->bool{self.blocked}fn block_chrome_owns_keyboard(&self,_:&egui::Context)->bool{false}fn active_terminal_is_read_only(&self)->bool{false} METHODS
fn switch(&mut self,index:usize){assert!(self.session_manager.switch_session(index));self.cancel_terminal_ime();self.active_session_epoch+=1;}
fn frame(&mut self,mut events:Vec<egui::Event>)->Vec<egui::Event>{events.retain(|event|{if let egui::Event::Key{pressed,..}=event{!self.native_enter_ownership.observe(*pressed)}else{true}});let ctx=egui::Context::default();ctx.input_mut(|i|i.pointer.latest=self.last_pointer_pos);let mut raw=egui::RawInput{focused:true,events};{let raw_input=&mut raw;PREAMBLE}self.prepare_terminal_ime_input(&ctx,&mut raw);self.frame_events=raw.events.clone();self.discard_stale_frame_ime();if !self.blocked{self.handle_ime_events(&ctx);}raw.events}
}
fn app()->App{App{last_pointer_pos:None,layout:Layout{panes:vec![Pane{rect:Rect(0.0,1.0),session_idx:0},Pane{rect:Rect(1.0,2.0),session_idx:1}]},session_manager:Manager{sessions:["A","B"].into_iter().map(|id|Session{terminal:Arc::new(parking_lot::Mutex(RefCell::new(Terminal::default()))),metadata:Metadata{session_id:id.into(),unseen_output:false},bytes:vec![]}).collect(),active_index:0,previous_session_id:None},frame_events:vec![],organism:Organism,status_message:String::new(),status_expires_at:None,active_session_epoch:1,terminal_ime:Default::default(),frame_ime_owner:None,ime_origin_terminal:None,native_enter_ownership:Default::default(),blocked:false}}
fn preedit(t:&str)->egui::Event{egui::Event::Ime(egui::ImeEvent::Preedit{text:t.into()})}fn commit()->egui::Event{egui::Event::Ime(egui::ImeEvent::Commit("你好".into()))}fn enter(pressed:bool)->egui::Event{egui::Event::Key{key:egui::Key::Enter,pressed,repeat:false}}
#[test]fn production_cross_pane_cancel_clears_origin_and_drops_late_commit(){let mut a=app();a.frame(vec![preedit("nihao")]);a.switch(1);assert!(a.session_manager.sessions[0].terminal.lock().preedit_text.is_empty());assert!(a.frame(vec![commit()]).is_empty());assert!(a.session_manager.sessions.iter().all(|s|s.bytes.is_empty()));assert_eq!(a.frame(vec![commit()]).len(),1);assert!(a.queue_ordered_ime_commit("你好"));assert_eq!(a.session_manager.sessions[1].bytes,"你好".as_bytes());}
#[test]fn production_commit_before_switch_keeps_order_and_after_switch_is_rejected(){let mut a=app();a.frame(vec![commit()]);assert!(a.queue_ordered_ime_commit("before"));a.switch(1);assert!(!a.queue_ordered_ime_commit("after"));assert_eq!(a.session_manager.sessions[0].bytes,b"before");assert!(a.session_manager.sessions[1].bytes.is_empty());}
#[test]fn production_deferred_encoder_discards_stale_commit_even_after_return(){let mut a=app();a.frame(vec![commit()]);a.switch(1);a.switch(0);a.discard_stale_frame_ime();assert!(a.frame_events.is_empty());}
#[test]fn production_raw_candidate_enter_precedes_modal_and_commit(){let mut a=app();let out=a.frame(vec![preedit("nihao"),enter(true),enter(true),commit()]);assert!(!out.iter().any(|e|matches!(e,egui::Event::Key{pressed:true,..})));assert_eq!(a.native_enter_ownership.claims,1);}
#[test]fn production_raw_commit_then_fresh_enter_stays_available(){let mut a=app();a.frame(vec![preedit("nihao")]);let out=a.frame(vec![commit(),enter(true)]);assert!(matches!(out.last(),Some(egui::Event::Key{pressed:true,..})));}
#[test]fn production_cancelled_overlay_commit_retires_then_new_ime_works(){let mut a=app();a.frame(vec![preedit("nihao")]);a.blocked=true;assert!(a.frame(vec![commit()]).is_empty());assert_eq!(a.frame(vec![preedit("new"),commit()]).len(),2);a.blocked=false;assert_eq!(a.frame(vec![preedit("fresh"),commit()]).len(),2);}
#[test]fn production_empty_preedit_retains_owner_and_cancelled_enter_not_stuck(){let mut a=app();a.frame(vec![preedit("nihao"),preedit("")]);a.switch(1);assert!(a.frame(vec![enter(true),enter(true)]).is_empty());let out=a.frame(vec![enter(false),enter(true)]);assert_eq!(out.len(),2);assert!(a.frame(vec![commit()]).is_empty());}
#[test]fn production_released_candidate_cannot_claim_later_fresh_native_hold(){let mut a=app();let out=a.frame(vec![preedit("nihao"),enter(true),enter(false),commit(),enter(true)]);assert_eq!(a.native_enter_ownership.claims,0);assert!(matches!(out.last(),Some(egui::Event::Key{pressed:true,..})));}
#[test]fn production_window_blur_invalidates_frame_commit_stamp(){let mut a=app();a.frame(vec![preedit("nihao")]);a.frame(vec![egui::Event::WindowFocused(false),commit(),commit()]);assert!(a.frame_ime_owner.is_none());assert!(!a.queue_ordered_ime_commit("late"));assert!(a.session_manager.sessions.iter().all(|s|s.bytes.is_empty()));}
#[test]fn production_cancelled_candidate_native_repeats_end_at_real_release(){let mut a=app();a.frame(vec![preedit("nihao")]);a.switch(1);assert!(a.frame(vec![enter(true)]).is_empty());assert!(a.frame(vec![enter(true)]).is_empty());assert_eq!(a.frame(vec![enter(false),enter(true)]).len(),2);assert!(a.frame(vec![commit()]).is_empty());}
fn click()->egui::Event{egui::Event::PointerButton{pressed:true,pos:Pos(0.5)}}
#[test]fn production_pointer_before_commit_sends_to_neither_pane_or_modal(){let mut a=app();a.frame(vec![preedit("nihao")]);let out=a.frame(vec![click(),commit()]);assert!(!out.iter().any(|e|matches!(e,egui::Event::Ime(_))));a.switch(1);assert!(a.session_manager.sessions.iter().all(|s|s.bytes.is_empty()));assert!(a.session_manager.sessions[0].terminal.lock().preedit_text.is_empty());}
#[test]fn production_commit_prefix_before_pointer_keeps_original_recipient(){let mut a=app();a.frame(vec![preedit("nihao")]);let out=a.frame(vec![commit(),click()]);assert!(matches!(out.first(),Some(egui::Event::Ime(egui::ImeEvent::Commit(_)))));assert!(a.queue_ordered_ime_commit("你好"));a.switch(1);assert_eq!(a.session_manager.sessions[0].bytes,"你好".as_bytes());assert!(a.session_manager.sessions[1].bytes.is_empty());}
#[test]fn production_pointer_cancels_same_batch_display_preedit_and_late_commit_after_return(){let mut a=app();let out=a.frame(vec![preedit("nihao"),click()]);assert!(!out.iter().any(|e|matches!(e,egui::Event::Ime(_))));assert!(a.session_manager.sessions[0].terminal.lock().preedit_text.is_empty());a.switch(1);a.switch(0);assert!(a.frame(vec![commit()]).is_empty());assert_eq!(a.frame(vec![preedit("fresh"),commit()]).len(),2);}
#[test]fn production_pointer_prefix_preedit_quarantines_later_commit(){let mut a=app();a.frame(vec![click(),preedit("late")]);a.switch(1);assert!(a.frame(vec![commit()]).is_empty());assert_eq!(a.frame(vec![commit()]).len(),1);}
#[test]fn production_pointer_commit_enter_never_activates_new_modal(){let mut a=app();a.frame(vec![preedit("nihao")]);let out=a.frame(vec![click(),commit(),enter(true)]);assert!(!out.iter().any(|e|matches!(e,egui::Event::Key{pressed:true,..}|egui::Event::Ime(_))));assert!(a.frame(vec![enter(true)]).is_empty());assert_eq!(a.frame(vec![enter(false),enter(true)]).len(),2);}
#[test]fn production_current_pane_wheel_keeps_composition_other_pane_wheel_cancels(){for other in [false,true]{let mut a=app();a.frame(vec![preedit("nihao")]);a.last_pointer_pos=Some(Pos(if other{1.5}else{0.5}));let out=a.frame(vec![egui::Event::MouseWheel{},commit()]);assert_eq!(out.iter().any(|e|matches!(e,egui::Event::Ime(_))),!other);}}
#[test]fn production_pointer_cancellation_without_commit_claims_only_one_hold(){let mut a=app();a.frame(vec![preedit("nihao")]);assert_eq!(a.frame(vec![click(),enter(true)]).len(),1);assert!(a.frame(vec![enter(true)]).is_empty());assert_eq!(a.frame(vec![enter(false),enter(true)]).len(),2);assert!(a.frame(vec![commit()]).is_empty());}
#[test]fn production_move_gone_wheel_uses_retained_interaction_position(){let mut a=app();a.last_pointer_pos=Some(Pos(0.5));a.frame(vec![preedit("nihao")]);let out=a.frame(vec![egui::Event::PointerMoved(Pos(1.5)),egui::Event::PointerGone,egui::Event::MouseWheel{},commit()]);assert!(a.last_pointer_pos.is_none());assert!(!out.iter().any(|e|matches!(e,egui::Event::Ime(_))));}
#[test]fn production_button_release_wheel_updates_interaction_without_hover(){let mut a=app();a.last_pointer_pos=Some(Pos(0.5));a.frame(vec![preedit("nihao")]);let out=a.frame(vec![egui::Event::PointerButton{pressed:false,pos:Pos(1.5)},egui::Event::MouseWheel{},commit()]);assert!(!out.iter().any(|e|matches!(e,egui::Event::Ime(_))));}
#[test]fn production_previous_hover_gone_wheel_retains_interaction(){let mut a=app();a.last_pointer_pos=Some(Pos(1.5));a.frame(vec![preedit("nihao")]);let out=a.frame(vec![egui::Event::PointerGone,egui::Event::MouseWheel{},commit()]);assert!(a.last_pointer_pos.is_none());assert!(!out.iter().any(|e|matches!(e,egui::Event::Ime(_))));}
#[test]fn production_empty_without_commit_recovers_fresh_enter_after_release(){let mut a=app();a.frame(vec![preedit("nihao"),preedit("")]);assert!(a.frame(vec![enter(true)]).is_empty());assert!(a.frame(vec![enter(true)]).is_empty());assert_eq!(a.frame(vec![enter(false),enter(true)]).len(),2);assert_eq!(a.frame(vec![commit()]).len(),1);assert!(a.queue_ordered_ime_commit("你好"));}
#[test]fn production_empty_keeps_owner_for_following_commit_across_switch(){let mut a=app();a.frame(vec![preedit("nihao"),preedit("")]);a.switch(1);assert!(a.frame(vec![commit()]).is_empty());assert!(a.session_manager.sessions.iter().all(|s|s.bytes.is_empty()));}
'''
main=(root/'src/main.rs').read_text()
raw_hook=method(main,'fn raw_input_hook(')
preamble=raw_hook[raw_hook.index('        for event in &raw_input.events {'):raw_hook.index('        // Modal/search/settings')]
source=fixture.replace('ROOT',str(root)).replace('SWITCH',sw).replace('METHODS','\n'.join(methods)).replace('PREAMBLE',preamble)
main=(root/'src/main.rs').read_text()
state=(root/'src/app/state.rs').read_text()
raw_hook=method(main,'fn raw_input_hook(')
assert raw_hook.index('egui::Event::PointerMoved(pos) => self.last_pointer_pos = Some(*pos)') < raw_hook.index('self.prepare_terminal_ime_input(ctx, raw_input);')
assert raw_hook.index('egui::Event::PointerGone => self.last_pointer_pos = None') < raw_hook.index('self.prepare_terminal_ime_input(ctx, raw_input);')
assert 'self.prepare_terminal_ime_input(ctx, raw_input);' in method(main,'fn raw_input_hook(')
assert 'self.cancel_terminal_ime();' in method(state,'pub(crate) fn bump_active_session_epoch(')
assert main.count('self.discard_stale_frame_ime();') == 2
assert 'self.sync_terminal_ime_owner(ctx, ctx.input(|input| input.focused));' in method(s,'pub fn handle_keybindings(')
variants=[('current',source)]
if args.check_regression:
    guarded=method(source,'fn queue_ordered_ime_commit(')
    guard_start=guarded.index('        if self.frame_ime_owner')
    guard_end=guarded.index('        println!',guard_start)
    # This restores precisely the old active-session writer, while retaining
    # all new upstream guards, demonstrating why delivery-time checking matters.
    old=guarded[:guard_start]+guarded[guard_end:]
    variants.append(('old-unguarded-writer',source.replace(guarded,old)))
    variants.append(('old-pointer-routing', source.replace(
        'if pointer_boundary && self.frame_ime_owner.is_some()',
        'if false && pointer_boundary && self.frame_ime_owner.is_some()')))
    variants.append(('old-hover-wheel-routing',source.replace(
        'let wheel_changes_pane = interaction_pos.is_some_and',
        'let wheel_changes_pane = self.last_pointer_pos.is_some_and')))
    reducer=(root/'src/terminal_ime.rs').read_text()
    old_empty=reducer.replace('Composition::Active { empty: false } => {', 'Composition::Active { .. } => {')
    variants.append(('old-empty-enter-trap',source.replace(
        '#[path="'+str(root)+'/src/terminal_ime.rs"]mod terminal_ime;',
        'mod terminal_ime {\n'+old_empty+'\n}')))

with tempfile.TemporaryDirectory(prefix='ember-ime-ownership-') as directory:
    directory=Path(directory)
    for name,variant in variants:
        path=directory/(name+'.rs');path.write_text(variant)
        binary=directory/name
        subprocess.run([os.environ.get('RUSTC','rustc'),'--edition','2021','--test',str(path),'-o',str(binary)],check=True)
        result=subprocess.run([str(binary)],capture_output=True,text=True)
        print(name+':\n'+result.stdout)
        if name=='current':
            assert result.returncode==0,result.stderr
        else:
            assert result.returncode!=0
            assert ({'old-empty-enter-trap':'production_empty_without_commit_recovers_fresh_enter_after_release', 'old-pointer-routing':'production_pointer_before_commit_sends_to_neither_pane_or_modal', 'old-hover-wheel-routing':'production_move_gone_wheel_uses_retained_interaction_position', 'old-unguarded-writer':'production_commit_before_switch_keeps_order_and_after_switch_is_rejected'}[name]+' ... FAILED') in result.stdout
print('Production IME routing and wiring checks passed.')
