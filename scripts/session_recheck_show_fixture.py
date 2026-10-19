"""Fake-egui boundary for executing exact recovery show/receiver logic.

No real rendering, window, export provider or filesystem metadata is used.
The independent reviewer authored the cases; each run extracts current methods.
"""
import json


def build_show_code(source, module_path):
    def item(marker):
        start=source.index(marker)
        opening=source.index('{',start)
        depth,end=1,opening+1
        while depth:
            depth+=(source[end]=='{')-(source[end]=='}')
            end+=1
        return source[start:end]
    methods=['fn new(', 'fn recheck(', 'fn poll_recheck(', 'fn cancel_recheck(', 'fn start_export(', 'pub fn show(']
    extracted='\n'.join(item(x) for x in methods)
    code='''#![allow(dead_code)]
    use std::{path::{Path,PathBuf},sync::{Arc,Mutex,mpsc,atomic::{AtomicUsize, Ordering}},time::{Duration,Instant}};
    mod review_text { pub fn bound_toast_text(text: String)->String {text} }
    mod egui {
        use super::*;
        #[derive(Default)] pub struct Frame { pub clicks: Vec<String>, pub close: bool, pub labels: Vec<String>, pub buttons: Vec<(String,bool)> }
        #[derive(Clone,Default)] pub struct Context { pub frame: Arc<Mutex<Frame>>, pub wakes: Arc<AtomicUsize> }
        impl Context {
            pub fn request_repaint(&self) { self.wakes.fetch_add(1,Ordering::SeqCst); }
            pub fn click(&self,s:&str) { self.frame.lock().unwrap().clicks.push(s.into()); }
            pub fn close(&self) { self.frame.lock().unwrap().close=true; }
            pub fn clear(&self) { let mut f=self.frame.lock().unwrap(); f.labels.clear(); f.buttons.clear(); }
        }
        pub struct Ui(Context);
        impl Ui {
            pub fn label(&mut self,s:impl AsRef<str>) {self.0.frame.lock().unwrap().labels.push(s.as_ref().into());}
            pub fn separator(&mut self) {}
            pub fn button(&mut self,s:&str)->Response {self.add_enabled(true,Button::new(s))}
            pub fn add_enabled(&mut self,e:bool,b:Button)->Response {
                let mut f=self.0.frame.lock().unwrap(); f.buttons.push((b.0.clone(),e));
                let pos=f.clicks.iter().position(|x| x==&b.0);
                let clicked=pos.is_some()&&e;
                if let Some(i)=pos {f.clicks.remove(i);}
                Response(clicked)
            }
            pub fn add(&mut self,_:TextEdit<'_>) {}
        }
        pub struct Response(bool); impl Response {pub fn clicked(&self)->bool{self.0}}
        pub struct Button(String); impl Button {pub fn new(s:&str)->Self{Self(s.into())}}
        pub struct TextEdit<'a>(&'a mut String); impl<'a> TextEdit<'a> {
            pub fn singleline(s:&'a mut String)->Self {Self(s)}
            pub fn hint_text(self,_:&str)->Self{self} pub fn char_limit(self,_:usize)->Self{self}
        }
        pub struct Id; impl Id {pub fn new(_:&str)->Self{Self}}
        pub struct Align2; impl Align2 {pub const LEFT_BOTTOM:Self=Self;}
        pub struct Area; impl Area {
            pub fn new(_:Id)->Self{Self} pub fn anchor(self,_:Align2,_:[f32;2])->Self{self}
            pub fn show(self,c:&Context,f:impl FnOnce(&mut Ui)){f(&mut Ui(c.clone()));}
        }
        pub struct Window<'a>{open:Option<&'a mut bool>}
        impl<'a> Window<'a> {
            pub fn new(_:&str)->Self{Self{open:None}} pub fn id(self,_:Id)->Self{self}
            pub fn open(mut self,o:&'a mut bool)->Self{self.open=Some(o);self}
            pub fn default_width(self,_:f32)->Self{self} pub fn vscroll(self,_:bool)->Self{self}
            pub fn show(mut self,c:&Context,f:impl FnOnce(&mut Ui)) {
                let close={let mut x=c.frame.lock().unwrap();std::mem::take(&mut x.close)};
                if close {if let Some(o)=self.open.as_mut(){**o=false;}return;}
                f(&mut Ui(c.clone()));
            }
        }
    }
    '''
    code += '#[path='+json.dumps(str(module_path))+'] mod recheck;\nuse recheck::AsyncRecheck;\n'
    code += '''#[derive(Debug,Clone,PartialEq,Eq)] struct RepairSummary {
        skipped_sessions:usize, session_fields:[usize;64],invalid_cwds:[bool;64],
        layout_adjusted:bool,selection_adjusted:bool,retained_sessions:usize,
    }
    struct Snapshot{sessions:Vec<()>}
    struct Gates{started:mpsc::Sender<()>,release:mpsc::Receiver<()>}
    static GATES:Mutex<Option<Gates>>=Mutex::new(None);
    static CALLS:AtomicUsize=AtomicUsize::new(0);
    static EXPORT_CALLS:AtomicUsize=AtomicUsize::new(0);
    fn summary()->RepairSummary {RepairSummary{skipped_sessions:1,session_fields:[0;64],invalid_cwds:[false;64],layout_adjusted:false,selection_adjusted:false,retained_sessions:2}}
    fn inspect_recovery(text: &str)->Result<(Snapshot,RepairSummary),()> {
        CALLS.fetch_add(1,Ordering::SeqCst);
        let g=GATES.lock().unwrap().take().expect("unplanned inspector");
        g.started.send(()).unwrap(); g.release.recv().unwrap();
        if text == "PARSE_PRIVATE_SENTINEL" { return Err(()); }
        Ok((Snapshot{sessions:vec![();2]},summary()))
    }
    fn export_original(_:&Path,_:&[u8])->std::io::Result<()> {EXPORT_CALLS.fetch_add(1,Ordering::SeqCst);Ok(())}
    '''
    code+=item('pub(crate) struct SessionRecovery')+'\nimpl SessionRecovery {\n'+extracted+'\n}\n'+item('fn recheck_notice(')+'\n'
    code += r'''static SERIAL: Mutex<()> = Mutex::new(());
    fn fixture()->SessionRecovery { SessionRecovery::new(Path::new("/synthetic-source"),Some(Arc::from(&b"synthetic metadata"[..])),Some(summary())) }
    fn begin(mut r:SessionRecovery, ctx:egui::Context)->(SessionRecovery,mpsc::Sender<()>) {
        let (started_tx,started_rx)=mpsc::channel(); let(release_tx,release_rx)=mpsc::channel();
        *GATES.lock().unwrap()=Some(Gates{started:started_tx,release:release_rx});
        let(returned_tx,returned_rx)=mpsc::channel();
        let invoker=std::thread::spawn(move || {ctx.click("Recheck original metadata");r.show(&ctx);returned_tx.send(r).unwrap_or_else(|_|panic!("invoker result receiver dropped"));});
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let returned=returned_rx.recv_timeout(Duration::from_secs(2));
        if returned.is_err(){let _=release_tx.send(());}
        invoker.join().unwrap();
        (returned.expect("exact show waited for inspector"),release_tx)
    }
    fn finish(r:&mut SessionRecovery,ctx:&egui::Context) {
        let deadline=Instant::now()+Duration::from_secs(5);
        while r.recheck.is_busy(){r.show(ctx);assert!(Instant::now()<deadline);std::thread::yield_now();}
    }
    fn export_channel(r:&mut SessionRecovery)->mpsc::Sender<Result<String,String>> {let(tx,rx)=mpsc::channel();r.pending=Some(rx);tx}
    #[test]
    fn show_receiver_orders_and_closed_cancel() {
        let _serial = SERIAL.lock().unwrap();
        for order in 0..2 {
            for result in [Ok("export success".to_owned()), Err("export failure".to_owned())] {
                let ctx=egui::Context::default(); let (mut r,release)=begin(fixture(),ctx.clone());
                let bytes=r.original.clone().unwrap(); let report=r.summary.clone();
                let tx=export_channel(&mut r);
                if order==0 {tx.send(result.clone()).unwrap();r.show(&ctx);assert!(r.pending.is_none());}
                release.send(()).unwrap(); finish(&mut r,&ctx);
                let rechecked=r.recheck_notice.clone();assert!(rechecked.starts_with("Recheck: 2 session records"));
                if order==1 {tx.send(result.clone()).unwrap();r.show(&ctx);}
                assert_eq!(r.export_notice,result.unwrap_or_else(|s|s)); assert_eq!(r.recheck_notice,rechecked);
                assert!(r.pending.is_none());assert!(Arc::ptr_eq(&bytes,r.original.as_ref().unwrap()));assert_eq!(report,r.summary);
            }
        }
        for disconnect_before in [true,false] {
            let ctx=egui::Context::default();let(mut r,release)=begin(fixture(),ctx.clone());let tx=export_channel(&mut r);
            if disconnect_before {drop(tx);r.show(&ctx);} else {release.send(()).unwrap();finish(&mut r,&ctx);drop(tx);}
            r.show(&ctx);assert!(r.export_notice.contains("stopped before reporting completion"));
            let export_notice=r.export_notice.clone();
            if disconnect_before {release.send(()).unwrap();finish(&mut r,&ctx);}
            assert_eq!(r.export_notice,export_notice);
        }
        // Closing an in-flight recheck with an export pending keeps export tracking alive.
        for close_by_x in [false,true] {
            let ctx=egui::Context::default();let(mut r,release)=begin(fixture(),ctx.clone());let calls=CALLS.load(Ordering::SeqCst);
            let tx=export_channel(&mut r);
            if close_by_x{ctx.close();}else{ctx.click("Dismiss summary");}
            r.show(&ctx);assert!(!r.open);assert!(r.recheck.is_busy());assert!(r.pending.is_some());
            let discarded=r.recheck_notice.clone();assert!(discarded.contains("discarded"));
            tx.send(Ok("closed export completion".into())).unwrap();r.show(&ctx);assert_eq!(r.export_notice,"closed export completion");assert!(r.pending.is_none());
            ctx.click("Session recovery details");r.show(&ctx);assert!(r.open);
            for _ in 0..12 {ctx.click("Recheck original metadata");r.show(&ctx);}
            assert_eq!(CALLS.load(Ordering::SeqCst),calls);
            assert!(ctx.frame.lock().unwrap().buttons.iter().any(|(s,enabled)|s=="Recheck original metadata"&&!*enabled));
            release.send(()).unwrap();finish(&mut r,&ctx);assert_eq!(r.recheck_notice,discarded);assert_eq!(r.export_notice,"closed export completion");
            let(mut r,release)=begin(r,ctx.clone());release.send(()).unwrap();finish(&mut r,&ctx);assert!(r.recheck_notice.starts_with("Recheck: 2 session records"));
        }
        assert_eq!(EXPORT_CALLS.load(Ordering::SeqCst),0);
    }
    #[test]
    fn show_discards_ready_result_before_poll_when_closed() {
        let _serial = SERIAL.lock().unwrap();
        let ctx=egui::Context::default();let(mut r,release)=begin(fixture(),ctx.clone());let wakes=ctx.wakes.load(Ordering::SeqCst);
        release.send(()).unwrap();let deadline=Instant::now()+Duration::from_secs(5);
        while ctx.wakes.load(Ordering::SeqCst)==wakes {assert!(Instant::now()<deadline);std::thread::yield_now();}
        r.open=false;r.show(&ctx);assert!(!r.recheck.is_busy());assert!(r.recheck_notice.contains("discarded"));assert!(!r.recheck_notice.starts_with("Recheck: 2"));
    }
    #[test]
    fn show_absent_closed_and_unreadable_original_are_safe() {
        let _serial = SERIAL.lock().unwrap();
        let ctx=egui::Context::default();let mut r=fixture();let calls=CALLS.load(Ordering::SeqCst);
        r.open=false;r.recheck(&ctx);assert!(!r.recheck.is_busy());assert_eq!(CALLS.load(Ordering::SeqCst),calls);
        r.open=true;r.original=None;ctx.click("Recheck original metadata");r.show(&ctx);r.recheck(&ctx);
        assert!(!r.recheck.is_busy());assert_eq!(CALLS.load(Ordering::SeqCst),calls);assert!(r.recheck_notice.is_empty());
        r.original=Some(Arc::from(&b"\xff\xfe"[..]));let original=r.original.clone().unwrap();let summary=r.summary.clone();
        r.recheck(&ctx);finish(&mut r,&ctx);
        assert_eq!(r.recheck_notice,"Recheck: original metadata is still unreadable. Live sessions unchanged.");
        assert!(Arc::ptr_eq(&original,r.original.as_ref().unwrap()));assert_eq!(r.summary,summary);assert_eq!(CALLS.load(Ordering::SeqCst),calls);
        r.original=Some(Arc::from(&b"PARSE_PRIVATE_SENTINEL"[..]));let(mut r,release)=begin(r,ctx.clone());release.send(()).unwrap();finish(&mut r,&ctx);
        assert_eq!(r.recheck_notice,"Recheck: original metadata is still unreadable. Live sessions unchanged.");assert!(!r.recheck_notice.contains("PARSE_PRIVATE_SENTINEL"));
    }
    '''
    return code
