#!/usr/bin/env python3
"""Actual terminal, picker state and extracted production controller; no GUI/native link.
UI painting/navigation are not simulated acceptance: assertions exercise the exact
production resolver before a small fixture records its returned target.
"""
import argparse, os, subprocess, tempfile
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('--deps',type=Path,required=True);p.add_argument('--regression',action='store_true');p.add_argument('--allocation-probe',action='store_true');p.add_argument('--compositor-probe',action='store_true');a=p.parse_args()
r=Path(__file__).resolve().parents[1];deps=a.deps.resolve();commands=(r/'src/app/commands.rs').read_text()
def section(start,end):
 i=commands.index(start);return commands[i:commands.index(end,i)]
refresh=section('    pub(crate) fn refresh_block_search_hits(', '    /// Capture only a bounded immutable source')
rebuild=section('    fn rebuild_block_search_cache(', '    /// A click/plain Enter')
resolve=section('    fn block_search_resolve_activation(', '    pub(crate) fn block_search_accept_target(')
accept=section('    pub(crate) fn block_search_accept(', '    /// Resolve only the exact rendered claim.')
assert 'command_output_text(' not in refresh+rebuild
assert 'extract_text_range(' not in refresh+rebuild
helpers=section('fn block_search_record_version(', '#[derive(Clone, Copy, Debug, PartialEq, Eq)]\nenum BlockSearchActivation')
helpers=helpers.replace('pub(super) fn','fn')
start=commands.index('fn defer_same_session_block_search_rebuild_if_invalid(');end=commands.index('\n}\n',start)+3;helpers+=commands[start:end]
helpers+=section('fn block_search_output_source(', '/// OSC prompt metadata')
source='#![allow(dead_code)]\npub use jterm_core::char_width;\n'+f'const COMPOSITOR_PROBE: bool = {str(a.compositor_probe).lower()};\n'
for m in ['terminal','block_mode','block_search','kitty_graphics','output_batch','review_text','fold_preview','debug']:
 path=r/'src'/('terminal/mod.rs' if m=='terminal' else m+'.rs');source+=f'#[path="{path}"] mod {m};\n'
source+=r'''
use block_search::*;
use std::collections::HashMap;
use std::sync::Arc;
use parking_lot::Mutex;
use terminal::TerminalState;
struct Metadata { session_id:String }
struct Session { terminal:Arc<Mutex<TerminalState>>, metadata:Metadata }
struct Manager { session:Session }
impl Manager { fn get_active_session_mut(&mut self)->&mut Session {&mut self.session} }
struct Config {notify_long_block_threshold_ms:u64}
struct App { block_search:BlockSearchState,block_bookmarks:BlockBookmarkState,session_manager:Manager,config:Config,activated:Vec<String> }
impl App {
'''+refresh+rebuild+resolve+accept+r'''
fn block_search_accept_target(&mut self, claim:BlockSearchActivationTarget,_keep:bool) {
 if let Some((_,hit))=self.block_search_resolve_activation(claim){self.activated.push(hit.record_id);}
}
}
'''+helpers+r'''
fn app()->App {
 let mut t=TerminalState::new(128,4);t.set_max_scrollback(150000);
 App{block_search:BlockSearchState::default(),block_bookmarks:BlockBookmarkState::default(),session_manager:Manager{session:Session{terminal:Arc::new(Mutex::new(t)),metadata:Metadata{session_id:"pane-a".into()}}},config:Config{notify_long_block_threshold_ms:1000},activated:vec![]}
}
fn record(a:&mut App,id:&str,lines:usize,needle:&str) {
 let mut t=a.session_manager.session.terminal.lock();
 t.process_input(format!("\x1b]133;A\x07\x1b]133;C;id={id}\x07").as_bytes());
 for i in 0..lines { if i==lines/2 {t.process_input(needle.as_bytes());} else {t.process_input(b"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx");} t.process_input(b"\r\n"); }
 t.process_input(format!("\x1b]133;D;0;id={id}\x07\r\n\r\n\r\n\r\n").as_bytes());
}
fn ready(a:&mut App) {
 let now=std::time::Instant::now();loop {a.refresh_block_search_hits();if !a.block_search.is_loading(){break;}assert!(now.elapsed()<std::time::Duration::from_secs(10));std::thread::sleep(std::time::Duration::from_millis(1));}
}
fn opened(a:&mut App,query:&str){a.block_search.open();a.block_search.query=query.into();ready(a);}
#[test]fn snapshot_fast_path_does_not_start_raw_worker(){let mut a=app();record(&mut a,"small",10,"needle");opened(&mut a,"needle");assert_eq!(a.block_search.hits.len(),1);assert!(a.block_search.raw.records.is_empty());assert!(a.block_search.raw.worker.is_none());assert!(a.block_search.raw.pool.is_none());}
#[test]fn raw_output_is_explicit_until_requested_and_continuation_precedes_other_records(){
 let mut a=app();a.block_search.raw.operation_limits.windows=1;record(&mut a,"large",6000,"middle-needle");opened(&mut a,"middle-needle");assert!(a.block_search.hits.is_empty());assert_eq!(a.block_search.raw.records.len(),1);assert!(a.block_search.history_label().contains("not searched"));assert!(a.block_search.count_label().contains("partial"));
 assert!(a.block_search.request_earlier_history());ready(&mut a);assert!(a.block_search.raw.displayed);assert!(a.block_search.raw.continuation.is_some());assert!(a.block_search.history_before.is_none());
 let mut windows=1;while a.block_search.raw.continuation.is_some(){assert!(a.block_search.request_earlier_history());ready(&mut a);windows+=1;assert!(windows<20);}
 assert_eq!(a.block_search.hits.len(),1);assert!(a.block_search.raw.status.contains("Complete for this retained output only"));a.block_search.mark_hits_rendered();a.block_search_accept(false);assert_eq!(a.activated,vec!["large"]);println!("raw controller completed 6000 lines in {windows} explicit windows");
}
#[test]fn old_painted_claim_cannot_activate_completed_but_unadopted_raw_result(){
 let mut a=app();record(&mut a,"large",6000,"needle");record(&mut a,"small",10,"needle");opened(&mut a,"needle");a.block_search.mark_hits_rendered();let claim=a.block_search.activation_target(0).unwrap();assert_eq!(claim.hit.record_id,"small");assert!(a.block_search.request_earlier_history());a.refresh_block_search_hits();std::thread::sleep(std::time::Duration::from_millis(60));a.block_search_accept(false);a.block_search_accept_target(claim.clone(),false);assert!(a.activated.is_empty());ready(&mut a);a.block_search_accept_target(claim,false);assert!(a.activated.is_empty());
}
#[test]fn regex_and_whole_word_raw_limits_visible_without_blocking_snapshot_pages(){
 for regex in [false,true]{let mut a=app();record(&mut a,"large",4000,"needle");opened(&mut a,"needle");a.block_search.regex=regex;a.block_search.whole_word=!regex;a.block_search.computed_query=None;ready(&mut a);assert!(a.block_search.history_label().contains("does not support regex or whole-word"));assert!(!a.block_search.raw_can_advance());assert!(a.block_search.raw.worker.is_none());a.block_search.older_not_indexed=true;assert!(a.block_search.request_earlier_history());assert!(a.block_search.history_before.is_some());}
}
#[test]fn closing_reopening_preserves_pool_without_terminal_strong_owner(){
 let mut a=app();record(&mut a,"large",6000,"needle");opened(&mut a,"needle");assert!(a.block_search.request_earlier_history());a.refresh_block_search_hits();let pool=a.block_search.raw.pool.as_ref().unwrap().clone();let before=std::time::Instant::now();a.block_search.close();assert!(before.elapsed()<std::time::Duration::from_millis(100));a.block_search.open();assert!(Arc::ptr_eq(a.block_search.raw.pool.as_ref().unwrap(),&pool));let source=Arc::downgrade(&a.session_manager.session.terminal);a.session_manager.session.terminal=Arc::new(Mutex::new(TerminalState::new(64,4)));let now=std::time::Instant::now();while source.upgrade().is_some(){assert!(now.elapsed()<std::time::Duration::from_secs(1));std::thread::sleep(std::time::Duration::from_millis(1));}
}
#[test]fn query_change_and_pane_switch_reject_old_raw_results(){
 let mut a=app();record(&mut a,"large",6000,"needle");opened(&mut a,"needle");assert!(a.block_search.request_earlier_history());a.refresh_block_search_hits();a.block_search.query="different".into();a.block_search.computed_query=None;ready(&mut a);assert!(!a.block_search.raw.displayed);assert!(a.block_search.hits.is_empty());
 assert!(a.block_search.request_earlier_history());a.refresh_block_search_hits();a.session_manager.session.terminal=Arc::new(Mutex::new(TerminalState::new(64,4)));a.session_manager.session.metadata.session_id="pane-b".into();ready(&mut a);assert_eq!(a.block_search.session_id.as_deref(),Some("pane-b"));assert!(a.block_search.hits.is_empty());assert!(a.block_search.raw.continuation.is_none());
}
#[test]fn continuous_pty_output_does_not_restart_sealed_record_search(){
 let mut a=app();record(&mut a,"large",6000,"middle-needle");opened(&mut a,"middle-needle");let start=std::time::Instant::now();let mut windows=0;
 loop {assert!(a.block_search.request_earlier_history());while a.block_search.is_loading(){a.session_manager.session.terminal.lock().process_input(b"live output\r\n");a.refresh_block_search_hits();std::thread::sleep(std::time::Duration::from_millis(1));assert!(start.elapsed()<std::time::Duration::from_secs(10));}windows+=1;if a.block_search.raw.continuation.is_none(){break;}assert!(windows<20);}
 assert_eq!(a.block_search.hits.len(),1);assert!(a.block_search.raw.status.contains("Complete"));println!("continuous-output controller ready {:?}, user actions={windows}, automatic windows={}",start.elapsed(),a.block_search.raw.operation.as_ref().unwrap().completed);
}
#[test]fn eviction_or_reset_prevents_activation_of_raw_hit(){
 let mut a=app();record(&mut a,"large",4000,"middle-needle");opened(&mut a,"middle-needle");loop{assert!(a.block_search.request_earlier_history());ready(&mut a);if a.block_search.raw.continuation.is_none(){break;}}
 a.block_search.mark_hits_rendered();let claim=a.block_search.activation_target(0).unwrap();a.session_manager.session.terminal.lock().process_input(b"\x1bc");a.block_search_accept_target(claim,false);assert!(a.activated.is_empty());
}

#[test]fn snapshot_replacement_cannot_activate_an_old_painted_index(){
 let mut a=app();record(&mut a,"old",10,"needle");opened(&mut a,"needle");a.block_search.mark_hits_rendered();let claim=a.block_search.activation_target(0).unwrap();
 record(&mut a,"new",10,"needle");a.refresh_block_search_hits();assert!(a.block_search.history_pending.is_some());
 let now=std::time::Instant::now();while !a.block_search.history_pending.as_ref().unwrap().worker.is_finished(){assert!(now.elapsed()<std::time::Duration::from_secs(3));std::thread::sleep(std::time::Duration::from_millis(1));}
 a.block_search_accept(false);a.block_search_accept_target(claim.clone(),false);assert!(a.activated.is_empty());assert!(a.block_search.history_pending.is_some());ready(&mut a);a.block_search_accept_target(claim,false);a.block_search_accept(false);assert!(a.activated.is_empty());let expected=a.block_search.hits[a.block_search.selected_index].record_id.clone();a.block_search.mark_hits_rendered();a.block_search_accept(false);assert_eq!(a.activated,vec![expected]);
}
#[test]fn completed_raw_record_then_next_record_does_not_skip_intra_record_coverage(){
 let mut a=app();record(&mut a,"older",4000,"old-needle");record(&mut a,"newer",4000,"new-needle");opened(&mut a,"needle");assert_eq!(a.block_search.raw.eligible.len(),2);
 assert!(a.block_search.request_earlier_history());ready(&mut a);assert_eq!(a.block_search.raw.current.as_ref().unwrap().record_id,"newer");
 while a.block_search.raw.continuation.is_some(){assert!(a.block_search.request_earlier_history());ready(&mut a);assert_eq!(a.block_search.raw.current.as_ref().unwrap().record_id,"newer");}
 assert_eq!(a.block_search.hits.len(),1);assert!(a.block_search.request_earlier_history());ready(&mut a);assert_eq!(a.block_search.raw.current.as_ref().unwrap().record_id,"older");
}
#[test]fn snapshot_paging_finds_early_record_in_128000_real_retained_lines(){
 let mut a=app();let start=std::time::Instant::now();for i in 0..128 {record(&mut a,&format!("record-{i}"),1000,if i==0{"EARLY_NEEDLE"}else{"ordinary"});}
 let bytes:usize=a.session_manager.session.terminal.lock().command_records().iter().filter_map(|r|r.captured_output.as_ref()).map(|o|o.text.len()).sum();assert!(bytes>8*1024*1024);assert!(bytes<=16*1024*1024);
 opened(&mut a,"EARLY_NEEDLE");assert!(a.block_search.hits.is_empty());assert!(a.block_search.older_not_indexed);assert!(a.block_search.raw.records.is_empty());assert!(a.block_search.request_earlier_history());ready(&mut a);assert_eq!(a.block_search.hits.len(),1);assert_eq!(a.block_search.hits[0].record_id,"record-0");assert!(a.block_search.raw.worker.is_none());println!("real snapshot paging: 128000 lines, {bytes} bytes, fixture+ready {:?}",start.elapsed());
}
#[test]fn full_100000_line_raw_controller_latency_reports_all_windows(){
 let mut a=app();record(&mut a,"huge",100000,"EARLY_RETAINED_HIT");opened(&mut a,"EARLY_RETAINED_HIT");assert_eq!(a.block_search.raw.records.len(),1);
 let start=std::time::Instant::now();let mut ui_turns=0;let mut calls=Vec::new();let mut first_match=None;
 assert!(a.block_search.request_earlier_history());
 while a.block_search.is_loading(){let now=std::time::Instant::now();a.refresh_block_search_hits();calls.push(now.elapsed());if !a.block_search.hits.is_empty(){first_match.get_or_insert(start.elapsed());}ui_turns+=1;std::thread::sleep(std::time::Duration::from_millis(if COMPOSITOR_PROBE{16}else{1}));assert!(start.elapsed()<std::time::Duration::from_secs(30));}
 let operation=a.block_search.raw.operation.as_ref().unwrap();let windows=operation.completed;assert_eq!(operation.submitted,windows);assert!(windows>1);assert!(a.block_search.raw.continuation.is_none());
 assert_eq!(a.block_search.hits.len(),1);assert!(a.block_search.raw.status.contains("Complete"));calls.sort();let p95=calls[(calls.len()*95/100).min(calls.len()-1)];println!("actual controller 100000 lines: total={:?}; first match={first_match:?}; automatic windows={windows}; user actions=1; observed turns={ui_turns}; UI poll p95={p95:?}; polling interval={}ms, NOT compositor/native latency",start.elapsed(),if COMPOSITOR_PROBE{16}else{1});
}

#[test]fn contended_terminal_defers_ui_poll_without_blocking_or_losing_result(){
 let mut a=app();record(&mut a,"large",4000,"needle");opened(&mut a,"needle");assert!(a.block_search.request_earlier_history());a.refresh_block_search_hits();
 let terminal=Arc::clone(&a.session_manager.session.terminal);let held=terminal.lock();let start=std::time::Instant::now();for _ in 0..100{a.refresh_block_search_hits();}assert!(start.elapsed()<std::time::Duration::from_millis(100));assert!(a.block_search.is_loading());drop(held);ready(&mut a);assert!(a.block_search.raw.displayed);assert!(a.block_search.raw.operation.as_ref().unwrap().completed>0);
}

fn operation(now:std::time::Instant,limits:RawOperationLimits)->RawSearchOperation {
 RawSearchOperation::new(1,BlockSearchState::default().raw_operation_intent(),BlockSearchHistoryCursor{sequence:1,record_id:"r".into()},limits,now)
}
fn progress(cells:usize,bytes:usize,encoded:usize)->terminal::search_worker::SearchProgress {
 terminal::search_worker::SearchProgress{generation:1,chunks:1,work:terminal::search_chunk::SearchChunkWork{cells_charged:cells,bytes_emitted:bytes,encoded_bytes_charged:encoded,rows_decoded:1,..Default::default()},hits:0,lock_misses:0,lock_wait:std::time::Duration::ZERO,capture_time:std::time::Duration::ZERO,match_time:std::time::Duration::ZERO,elapsed:std::time::Duration::ZERO}
}
#[test]fn operation_budget_exact_boundaries_reservations_and_overflow_fail_closed(){
 let now=std::time::Instant::now();let w=terminal::search_worker::SearchWorkerLimits::default();
 let limits=RawOperationLimits{windows:1,cells:w.max_cells,bytes:w.max_bytes,encoded_bytes:w.max_encoded_bytes,duration:w.max_duration};
 let mut op=operation(now,limits);assert!(op.reserve(now).is_ok());assert_eq!(op.reserve(now),Err("window in flight"));assert!(op.charge(&progress(w.max_cells,w.max_bytes,w.max_encoded_bytes)));assert_eq!(op.reserve(now),Err("window limit"));assert!(!op.charge(&progress(0,0,0)));
 for selector in 0..4{let mut limits=limits;match selector{0=>limits.cells-=1,1=>limits.bytes-=1,2=>limits.encoded_bytes-=1,_=>limits.duration-=std::time::Duration::from_nanos(1)}let mut op=operation(now,limits);assert!(op.reserve(now).is_err());assert_eq!(op.submitted,0);}
 let mut op=operation(now,RawOperationLimits::default());op.cells=usize::MAX;assert_eq!(op.reserve(now),Err("cell limit"));
 let mut op=operation(now,RawOperationLimits::default());assert!(op.reserve(now).is_ok());assert!(!op.charge(&progress(w.max_cells+1,0,0)));assert_eq!(op.completed,0);
}
#[test]fn operation_deadline_counts_waiting_and_global_caps_cannot_expand(){
 let now=std::time::Instant::now();let huge=RawOperationLimits{windows:usize::MAX,cells:usize::MAX,bytes:usize::MAX,encoded_bytes:usize::MAX,duration:std::time::Duration::MAX};let mut op=operation(now,huge);
 assert_eq!(op.limits.windows,128);assert_eq!(op.limits.cells,16*1024*1024);assert_eq!(op.limits.bytes,16*1024*1024);assert_eq!(op.limits.encoded_bytes,64*1024*1024);
 assert!(op.allowance(now+std::time::Duration::from_millis(9500)).is_ok());assert_eq!(op.reserve(now+std::time::Duration::from_millis(9501)),Err("time limit"));assert_eq!(op.submitted,0);
}
#[test]fn natural_operation_limit_preserves_continuation_and_continue_never_restarts_matcher(){
 let mut a=app();a.block_search.raw.operation_limits.windows=2;record(&mut a,"large",6000,"needle");opened(&mut a,"needle");assert!(a.block_search.request_earlier_history());ready(&mut a);
 let op=a.block_search.raw.operation.as_ref().unwrap();assert_eq!(op.completed,2);assert_eq!(op.submitted,2);assert!(!op.active);assert!(a.block_search.raw.continuation.is_some());assert!(a.block_search.raw.status.contains("operation window limit"));let worker=a.block_search.raw.worker.as_ref().unwrap() as *const _ as usize;
 let mut actions=1;while a.block_search.raw.continuation.is_some(){assert!(a.block_search.request_earlier_history());ready(&mut a);actions+=1;assert_eq!(a.block_search.raw.worker.as_ref().unwrap() as *const _ as usize,worker);assert!(actions<10);}
 assert_eq!(actions,3);assert_eq!(a.block_search.hits.len(),1);assert_eq!(a.block_search.raw.next,1);
}
#[test]fn stop_after_first_matches_requires_repaint_then_restart_is_explicit(){
 let mut a=app();record(&mut a,"large",6000,"needle");opened(&mut a,"needle");assert!(a.block_search.request_earlier_history());let now=std::time::Instant::now();
 while a.block_search.hits.is_empty(){a.refresh_block_search_hits();assert!(now.elapsed()<std::time::Duration::from_secs(5));std::thread::sleep(std::time::Duration::from_millis(1));}
 assert!(a.block_search.is_loading());a.refresh_block_search_hits();assert!(a.block_search.raw.pending.is_some());a.block_search.mark_hits_rendered();assert!(a.block_search.activation_target(0).is_none());let visible=a.block_search.hits.clone();let worker=a.block_search.raw.worker.as_ref().unwrap() as *const _ as usize;
 a.block_search.stop_raw_operation();assert_eq!(a.block_search.hits,visible);assert!(a.block_search.raw.continuation.is_none());assert_eq!(a.block_search.earlier_label(),"Restart this output");assert!(!a.block_search.is_loading());a.block_search_accept(false);assert!(a.activated.is_empty());a.block_search.mark_hits_rendered();let old=a.block_search.activation_target(0).unwrap();let activate=std::time::Instant::now();while a.activated.is_empty(){a.block_search_accept_target(old.clone(),false);assert!(activate.elapsed()<std::time::Duration::from_secs(1));std::thread::sleep(std::time::Duration::from_millis(1));}assert_eq!(a.activated,vec!["large"]);
 assert!(a.block_search.request_earlier_history());a.block_search_accept_target(old,false);assert_eq!(a.activated.len(),1);ready(&mut a);assert_eq!(a.block_search.hits.len(),1);assert!(a.block_search.raw.continuation.is_none());assert_eq!(a.block_search.raw.worker.as_ref().unwrap() as *const _ as usize,worker);assert_eq!(a.block_search.raw.next,1);
}
#[test]fn stop_before_admission_then_restart_does_not_skip_record(){
 let mut a=app();record(&mut a,"large",4000,"needle");opened(&mut a,"needle");assert!(a.block_search.request_earlier_history());a.block_search.stop_raw_operation();assert!(a.block_search.raw.worker.is_none());assert_eq!(a.block_search.raw.next,1);assert_eq!(a.block_search.earlier_label(),"Restart this output");assert!(a.block_search.request_earlier_history());ready(&mut a);assert_eq!(a.block_search.hits.len(),1);assert_eq!(a.block_search.raw.next,1);
}
#[test]fn exhausted_allowance_cannot_submit_or_silently_restart(){
 let mut a=app();a.block_search.raw.operation_limits.windows=0;record(&mut a,"large",4000,"needle");opened(&mut a,"needle");assert!(a.block_search.request_earlier_history());ready(&mut a);assert!(a.block_search.raw.worker.is_none());assert_eq!(a.block_search.raw.operation.as_ref().unwrap().submitted,0);assert_eq!(a.block_search.earlier_label(),"Restart this output");for _ in 0..20{a.refresh_block_search_hits();}assert!(a.block_search.raw.worker.is_none());assert!(!a.block_search.is_loading());
}
#[test]fn busy_admission_does_not_consume_reservation_and_stop_invalidates_wait(){
 let mut a=app();record(&mut a,"large",4000,"needle");opened(&mut a,"needle");let pool=Arc::new(terminal::search_worker::SearchWorkerPermitPool::new(1));let blocker=terminal::search_worker::SearchWorker::new(Arc::downgrade(&a.session_manager.session.terminal),pool.clone(),Default::default()).unwrap();assert!(a.block_search.request_earlier_history());a.block_search.raw.pool=Some(pool.clone());a.refresh_block_search_hits();assert_eq!(a.block_search.raw.operation.as_ref().unwrap().submitted,0);assert!(a.block_search.raw.status.contains("Waiting"));a.block_search.stop_raw_operation();blocker.close();let now=std::time::Instant::now();while pool.active_workers()>0{assert!(now.elapsed()<std::time::Duration::from_secs(1));std::thread::sleep(std::time::Duration::from_millis(1));}for _ in 0..5{a.refresh_block_search_hits();}assert!(a.block_search.raw.worker.is_none());assert_eq!(a.block_search.earlier_label(),"Restart this output");assert!(a.block_search.request_earlier_history());ready(&mut a);assert_eq!(a.block_search.hits.len(),1);
}

#[test]fn reviewer_latest_during_contended_raw_completion_preserves_identity(){
 let mut a=app();record(&mut a,"large",6000,"needle");record(&mut a,"small",10,"needle");opened(&mut a,"needle");a.block_search.mark_hits_rendered();let old=a.block_search.activation_target(0).unwrap();
 assert!(a.block_search.request_earlier_history());a.refresh_block_search_hits();let pool=a.block_search.raw.pool.as_ref().unwrap().clone();std::thread::sleep(std::time::Duration::from_millis(80));
 let terminal=a.session_manager.session.terminal.clone();let held=terminal.lock();a.refresh_block_search_hits();assert!(a.block_search.request_latest_history());assert!(!a.block_search.contains_activation_target(&old));drop(held);ready(&mut a);
 assert!(Arc::ptr_eq(&pool,a.block_search.raw.pool.as_ref().unwrap()));assert!(!a.block_search.raw.displayed);assert!(a.block_search.raw.continuation.is_none());assert_eq!(a.block_search.hits.len(),1);assert_eq!(a.block_search.hits[0].record_id,"small");a.block_search_accept_target(old,false);assert!(a.activated.is_empty());
}
#[test]fn reviewer_invalidated_raw_hit_stays_unactivatable_after_repaint(){
 let mut a=app();record(&mut a,"large",4000,"needle");opened(&mut a,"needle");loop{assert!(a.block_search.request_earlier_history());ready(&mut a);if a.block_search.raw.continuation.is_none(){break;}}
 a.block_search.mark_hits_rendered();assert!(a.block_search.activation_target(0).is_some());a.session_manager.session.terminal.lock().on_resize(127,4);a.refresh_block_search_hits();assert!(a.block_search.raw.seal.is_none());a.block_search.mark_hits_rendered();assert!(a.block_search.activation_target(0).is_none());a.block_search_accept(false);assert!(a.activated.is_empty());
}

#[test]fn closed_worker_resume_drops_token_and_requires_explicit_restart_without_budget_overspend(){
 let mut a=app();a.block_search.raw.operation_limits.windows=1;record(&mut a,"large",4000,"needle");opened(&mut a,"needle");assert!(a.block_search.request_earlier_history());ready(&mut a);assert!(a.block_search.raw.continuation.is_some());a.block_search.raw.worker.as_ref().unwrap().close();
 assert!(a.block_search.request_earlier_history());ready(&mut a);let op=a.block_search.raw.operation.as_ref().unwrap();assert!(!op.active);assert_eq!(op.submitted,1);assert_eq!(op.completed,0);assert_eq!(op.cells,0);assert!(a.block_search.raw.continuation.is_none());assert_eq!(a.block_search.earlier_label(),"Restart this output");assert!(a.block_search.raw.worker.is_none());a.block_search.raw.operation_limits=Default::default();assert!(a.block_search.request_earlier_history());ready(&mut a);assert_eq!(a.block_search.hits.len(),1);
}
#[test]fn changing_options_without_computed_marker_still_invalidates_operation_intent(){
 let mut a=app();record(&mut a,"large",4000,"needle");opened(&mut a,"needle");assert!(a.block_search.request_earlier_history());a.refresh_block_search_hits();a.block_search.case_sensitive=true;a.refresh_block_search_hits();assert!(a.block_search.raw.operation.is_none());assert!(!a.block_search.raw.displayed);assert!(a.block_search.raw.continuation.is_none());
}

#[test]fn timeout_before_seal_is_explicit_restart_not_false_complete_or_automatic_loop(){
 let mut a=app();record(&mut a,"large",4000,"needle");opened(&mut a,"needle");assert!(a.block_search.request_earlier_history());let terminal=a.session_manager.session.terminal.clone();let held=terminal.lock();
 // Model PTY acquiring the source between refresh's initial try_lock and submit.
 a.refresh_block_search_raw("pane-a");std::thread::sleep(std::time::Duration::from_millis(600));drop(held);ready(&mut a);
 assert!(!a.block_search.raw.operation.as_ref().unwrap().active);assert_eq!(a.block_search.raw.operation.as_ref().unwrap().submitted,1);assert!(a.block_search.raw.continuation.is_none());assert!(a.block_search.raw.status.contains("Interrupted"));assert_eq!(a.block_search.earlier_label(),"Restart this output");assert!(a.block_search.activation_target(0).is_none());
 assert!(a.block_search.request_earlier_history());ready(&mut a);assert_eq!(a.block_search.hits.len(),1);
}

#[test]fn zero_progress_windows_still_exhaust_the_global_deadline(){
 let start=std::time::Instant::now();let mut op=operation(start,RawOperationLimits::default());let mut now=start;let mut count=0;
 while op.reserve(now).is_ok(){assert!(op.charge(&progress(0,0,0)));count+=1;now+=std::time::Duration::from_millis(500);assert!(count<=20);}
 assert_eq!(count,20);assert_eq!(op.allowance(now),Err("time limit"));assert_eq!(op.cells,0);
}
'''
if a.allocation_probe:
 source+=r"""
struct AllocationTracking;
static OWNER:std::sync::atomic::AtomicUsize=std::sync::atomic::AtomicUsize::new(0);
static STACK_MAX:std::sync::atomic::AtomicUsize=std::sync::atomic::AtomicUsize::new(0);
thread_local!{static STACK_CHECKED:std::cell::Cell<bool>=const{std::cell::Cell::new(false)};}
fn inspect_worker_stack(){
 let owner=OWNER.load(std::sync::atomic::Ordering::Relaxed);
 if owner==0 || unsafe{libc::pthread_self()} as usize==owner{return;}
 STACK_CHECKED.with(|checked|{if checked.replace(true){return;}unsafe{
  let mut attr=std::mem::MaybeUninit::<libc::pthread_attr_t>::uninit();
  if libc::pthread_getattr_np(libc::pthread_self(),attr.as_mut_ptr())==0{
   let mut attr=attr.assume_init();let mut size=0;
   if libc::pthread_attr_getstacksize(&attr,&mut size)==0{STACK_MAX.fetch_max(size,std::sync::atomic::Ordering::Relaxed);}
   libc::pthread_attr_destroy(&mut attr);
  }
 }});
}
static LIVE:std::sync::atomic::AtomicUsize=std::sync::atomic::AtomicUsize::new(0);
static PEAK:std::sync::atomic::AtomicUsize=std::sync::atomic::AtomicUsize::new(0);
unsafe impl std::alloc::GlobalAlloc for AllocationTracking {
 unsafe fn alloc(&self,l:std::alloc::Layout)->*mut u8 {inspect_worker_stack();let p=std::alloc::System.alloc(l);if !p.is_null(){let n=LIVE.fetch_add(l.size(),std::sync::atomic::Ordering::SeqCst)+l.size();PEAK.fetch_max(n,std::sync::atomic::Ordering::SeqCst);}p}
 unsafe fn dealloc(&self,p:*mut u8,l:std::alloc::Layout){LIVE.fetch_sub(l.size(),std::sync::atomic::Ordering::SeqCst);std::alloc::System.dealloc(p,l)}
 unsafe fn realloc(&self,p:*mut u8,l:std::alloc::Layout,n:usize)->*mut u8 {let q=std::alloc::System.realloc(p,l,n);if !q.is_null(){let v=if n>=l.size(){LIVE.fetch_add(n-l.size(),std::sync::atomic::Ordering::SeqCst)+n-l.size()}else{LIVE.fetch_sub(l.size()-n,std::sync::atomic::Ordering::SeqCst)-(l.size()-n)};PEAK.fetch_max(v,std::sync::atomic::Ordering::SeqCst);}q}
}
#[global_allocator]static ALLOC:AllocationTracking=AllocationTracking;
#[test]fn controller_allocation_with_retained_snapshot_cache_and_continuation(){
 for query in ["q".repeat(4096), "x".to_owned()] {
  let mut a=app();a.session_manager.session.terminal.lock().set_max_scrollback(200000);record(&mut a,"raw",100000,"needle");for i in 0..64{record(&mut a,&format!("captured-{i}"),1000,"ordinary");}opened(&mut a,&query);
  let cache:usize=a.block_search.cache.iter().map(|r|r.output.as_ref().map_or(0,String::capacity)+r.output_lowercase.as_ref().map_or(0,String::capacity)).sum();assert!(cache>8*1024*1024);
  let baseline=LIVE.load(std::sync::atomic::Ordering::SeqCst);PEAK.store(baseline,std::sync::atomic::Ordering::SeqCst);STACK_MAX.store(0,std::sync::atomic::Ordering::SeqCst);OWNER.store(unsafe{libc::pthread_self()} as usize,std::sync::atomic::Ordering::SeqCst);let mut windows=0;let mut automatic_windows=0;let mut limits=Vec::new();
  loop{assert!(a.block_search.request_earlier_history());ready(&mut a);windows+=1;automatic_windows+=a.block_search.raw.operation.as_ref().unwrap().completed;if a.block_search.raw.continuation.is_none(){break;}assert!(!a.block_search.raw.operation.as_ref().unwrap().active);assert!(a.block_search.raw.status.contains("operation"));limits.push(a.block_search.raw.status.clone());assert!(windows<20);}
  let extra=PEAK.load(std::sync::atomic::Ordering::SeqCst).saturating_sub(baseline);assert!(extra<8*1024*1024);assert_eq!(cache,a.block_search.cache.iter().map(|r|r.output.as_ref().map_or(0,String::capacity)+r.output_lowercase.as_ref().map_or(0,String::capacity)).sum::<usize>());
  let stack=STACK_MAX.load(std::sync::atomic::Ordering::SeqCst);OWNER.store(0,std::sync::atomic::Ordering::SeqCst);assert!(stack>0);assert!(extra+stack<8*1024*1024);println!("integrated allocator: querybytes={}, retained cache={}, additional heap peak={}, observed worker stack reservation={}, heap+stack reservation={}, user actions={}, automatic windows={}, visiblehits={}, natural limits={limits:?}; native RSS/graphics excluded",query.len(),cache,extra,stack,extra+stack,windows,automatic_windows,a.block_search.hits.len());a.block_search.close();let now=std::time::Instant::now();while a.block_search.raw.pool.as_ref().unwrap().active_workers()>0{assert!(now.elapsed()<std::time::Duration::from_secs(1));std::thread::sleep(std::time::Duration::from_millis(1));}
 }
}
"""
with tempfile.TemporaryDirectory(prefix='ember-ui-history-') as temp:
 t=Path(temp);(t/'test.rs').write_text(source)
 cmd=[os.environ.get('RUSTC','rustc'),'--edition=2021','--test','-C','debuginfo=0',str(t/'test.rs'),'-L','dependency='+str(deps)]
 for crate in ['unicode_normalization','uuid','parking_lot','jterm_core','base64','smallvec','unicode_width','regex','log','image','flate2','serde','serde_json','memchr','libc']:
  choices=sorted(deps.glob('lib'+crate+'-*.rlib'),key=lambda p:p.stat().st_mtime,reverse=True);cmd+=['--extern',crate+'='+str(choices[0])]
 cmd+=['-o',str(t/'tests')];subprocess.run(cmd,check=True)
 subprocess.run([str(t/'tests'),'full_100000','--test-threads=1','--nocapture'] if a.compositor_probe else [str(t/'tests'),'controller_allocation','--test-threads=1','--nocapture'] if a.allocation_probe else [str(t/'tests'),'--test-threads=1','--nocapture'] if a.regression else [str(t/'tests'),'--skip','terminal::','--skip','block_mode::','--test-threads=1','--nocapture'],check=True)
