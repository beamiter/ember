"""Run the real pointer command loop with a recording protocol shim; no display."""
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
from types import SimpleNamespace
import unittest

ROOT=Path(__file__).parent
spec=importlib.util.spec_from_file_location('desktop_pointer',ROOT/'native-desktop-smoke.py')
desktop=importlib.util.module_from_spec(spec);spec.loader.exec_module(desktop)

CLIENT=r'''
#include <stdint.h>
#include <stdio.h>
struct wl_display { int unused; }; struct wl_registry { int unused; };
struct wl_interface { int unused; };
struct wl_registry_listener {
 void (*global)(void*,struct wl_registry*,uint32_t,const char*,uint32_t);
 void (*global_remove)(void*,struct wl_registry*,uint32_t);
};
#define WL_POINTER_BUTTON_STATE_PRESSED 1
#define WL_POINTER_BUTTON_STATE_RELEASED 0
static inline struct wl_display *wl_display_connect(const char *s){(void)s;static struct wl_display d;return &d;}
static inline struct wl_registry *wl_display_get_registry(struct wl_display *d){(void)d;static struct wl_registry r;return &r;}
static inline void wl_registry_add_listener(struct wl_registry *r,const struct wl_registry_listener *l,void *d){l->global(d,r,1,"zwlr_virtual_pointer_manager_v1",1);}
static inline void *wl_registry_bind(struct wl_registry *r,uint32_t n,const struct wl_interface *i,uint32_t v){(void)r;(void)n;(void)i;(void)v;static int p;return &p;}
static inline int wl_display_roundtrip(struct wl_display *d){(void)d;static int calls;calls++;fprintf(stderr,"roundtrip\n");return getenv("FAIL_MOTION_ROUNDTRIP") && calls==3 ? -1 : 0;}
static inline void wl_registry_destroy(struct wl_registry *r){(void)r;}
static inline void wl_display_disconnect(struct wl_display *d){(void)d;}
'''
PROTOCOL=r'''
struct zwlr_virtual_pointer_manager_v1 { int unused; };
struct zwlr_virtual_pointer_v1 { int unused; };
static const struct wl_interface zwlr_virtual_pointer_manager_v1_interface={0};
static inline struct zwlr_virtual_pointer_v1 *zwlr_virtual_pointer_manager_v1_create_virtual_pointer(struct zwlr_virtual_pointer_manager_v1 *m,void *s){(void)m;(void)s;static struct zwlr_virtual_pointer_v1 p;return &p;}
static inline void zwlr_virtual_pointer_v1_motion_absolute(struct zwlr_virtual_pointer_v1 *p,uint32_t t,uint32_t x,uint32_t y,uint32_t w,uint32_t h){(void)p;(void)t;fprintf(stderr,"motion %u %u %u %u\n",x,y,w,h);}
static inline void zwlr_virtual_pointer_v1_frame(struct zwlr_virtual_pointer_v1 *p){(void)p;fprintf(stderr,"frame\n");}
static inline void zwlr_virtual_pointer_v1_button(struct zwlr_virtual_pointer_v1 *p,uint32_t t,uint32_t b,uint32_t s){(void)p;(void)t;fprintf(stderr,"button %u %u\n",b,s);}
static inline void zwlr_virtual_pointer_v1_destroy(struct zwlr_virtual_pointer_v1 *p){(void)p;}
static inline void zwlr_virtual_pointer_manager_v1_destroy(struct zwlr_virtual_pointer_manager_v1 *m){(void)m;}
'''

class NativeCommandTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp=tempfile.TemporaryDirectory();root=Path(cls.temp.name)
        (root/'wayland-client.h').write_text(CLIENT)
        (root/'wlr-virtual-pointer-unstable-v1-client-protocol.h').write_text(PROTOCOL)
        cls.binary=root/'pointer'
        subprocess.run(['cc','-Wall','-Wextra','-Werror','-I',str(root),str(ROOT/'native-pointer.c'),'-o',str(cls.binary)],check=True)
    @classmethod
    def tearDownClass(cls):cls.temp.cleanup()
    def run_command(self,command):
        return subprocess.run([str(self.binary)],input=command,text=True,capture_output=True,timeout=5)
    def test_move_only_motion_frame_roundtrip_then_one_ack(self):
        result=self.run_command('move 0 0 1280 900\n')
        self.assertEqual(result.returncode,0)
        self.assertEqual(result.stdout.splitlines(),['ready','done'])
        self.assertEqual(result.stderr.splitlines()[2:],['motion 0 0 1280 900','frame','roundtrip'])
        self.assertNotIn('button',result.stderr)
    def test_click_retains_press_release_sequence(self):
        result=self.run_command('click 40 50 1280 900\n')
        self.assertEqual(result.returncode,0)
        self.assertEqual(result.stdout.splitlines(),['ready','done'])
        self.assertEqual(result.stderr.splitlines()[2:],['motion 40 50 1280 900','frame','roundtrip',
            'button 272 1','frame','roundtrip','button 272 0','frame','roundtrip'])
    def test_compositor_roundtrip_failure_never_acknowledges_or_clicks(self):
        result=subprocess.run([str(self.binary)],input='move 0 0 1280 900\n',text=True,
                              capture_output=True,timeout=5,env=dict(os.environ,FAIL_MOTION_ROUNDTRIP='1'))
        self.assertEqual(result.returncode,2)
        self.assertEqual(result.stdout.splitlines(),['ready'])
        self.assertNotIn('button',result.stderr)

    def test_unknown_out_of_bounds_or_extra_fields_never_act_or_ack(self):
        for command in ['hover 0 0 1280 900','move 1280 0 1280 900','move 0 900 1280 900',
                        'move 0 0 0 900','move 0 0 1280 0','move -1 0 1280 900',
                        'move 0 0 1280 900 extra','move 0 0 1280','click 0 0 1280 900 extra',
                        'move 4294967296 0 1280 900','move 0 0 18446744073709551616 900',
                        'move 0 0 1280 900'+' '*150+'extra']:
            with self.subTest(command=command):
                result=self.run_command(command+'\n')
                self.assertEqual(result.returncode,2)
                self.assertEqual(result.stdout.splitlines(),['ready'])
                self.assertNotIn('motion',result.stderr);self.assertNotIn('button',result.stderr)

class ParkingTests(unittest.TestCase):
    def fixture(self,directory):
        output={'active':True,'scale':1,'transform':'normal','rect':{'x':0,'y':0,'width':1280,'height':900}}
        node={'app_id':'io.github.beamiter.ember','id':5,'pid':42,'focused':True,
              'rect':{'x':0,'y':25,'width':1280,'height':875}}
        stream=io.StringIO();acks=[]
        def run(args,**kwargs):return SimpleNamespace(stdout=json.dumps([output] if 'get_outputs' in args else node))
        smoke=SimpleNamespace(backend='wayland',out=Path(directory),app=SimpleNamespace(pid=42,poll=lambda:None),
                              pointer=SimpleNamespace(stdin=stream),pointer_ack=acks.append,run=run)
        return smoke,output,node,stream,acks
    def test_park_uses_actual_nonclient_corner_without_click(self):
        with tempfile.TemporaryDirectory() as d:
            smoke,output,node,stream,acks=self.fixture(d)
            desktop.Smoke.park_pointer(smoke)
            self.assertEqual(stream.getvalue(),'move 0 0 1280 900\n');self.assertEqual(acks,['done'])
            proof=json.loads((Path(d)/'ime-pointer-park.json').read_text())
            self.assertEqual(proof['owned']['pid'],42);self.assertTrue(proof['motion_only'])
    def test_invalid_output_owner_or_no_nonclient_corner_never_moves(self):
        for case in ['scale','bounds','owner','focus','client-corner','dead']:
            with self.subTest(case=case),tempfile.TemporaryDirectory() as d:
                smoke,output,node,stream,acks=self.fixture(d)
                if case=='scale':output['scale']=2
                elif case=='bounds':output['rect']['width']=1920
                elif case=='owner':node['pid']=99
                elif case=='focus':node['focused']=False
                elif case=='dead':smoke.app.poll=lambda:1
                else:node['rect']={'x':0,'y':0,'width':1280,'height':900}
                with self.assertRaises(RuntimeError):desktop.Smoke.park_pointer(smoke)
                self.assertEqual(stream.getvalue(),'');self.assertEqual(acks,[])
                self.assertFalse((Path(d)/'ime-pointer-park.json').exists())
    def test_standalone_ime_parks_once_before_raw_and_visual_baseline(self):
        source=(ROOT/'native-ime-return-smoke.py').read_text()
        self.assertEqual(source.count('smoke.park_pointer()'),1)
        self.assertLess(source.index('smoke.park_pointer()'),source.index('smoke.raw_start('))
        self.assertLess(source.index('smoke.park_pointer()'),source.index('before = visual.snapshot('))

    def test_failed_ack_never_claims_parking_complete(self):
        with tempfile.TemporaryDirectory() as d:
            smoke,output,node,stream,acks=self.fixture(d)
            def fail(_):raise RuntimeError('bad pointer ack')
            smoke.pointer_ack=fail
            with self.assertRaisesRegex(RuntimeError,'bad pointer ack'):desktop.Smoke.park_pointer(smoke)
            self.assertEqual(stream.getvalue(),'move 0 0 1280 900\n')
            self.assertFalse((Path(d)/'ime-pointer-park.json').exists())

if __name__=='__main__':unittest.main()
