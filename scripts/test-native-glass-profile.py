"""Pure glass observation registration and paint negatives; no native/IME run."""
import importlib.util
import json
import os
from pathlib import Path
import re
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
from PIL import Image, ImageDraw

spec = importlib.util.spec_from_file_location('glass_profile_test', Path(__file__).with_name('native_glass_profile.py'))
g = importlib.util.module_from_spec(spec); spec.loader.exec_module(g)

def module(name, file):
    spec=importlib.util.spec_from_file_location(name,Path(__file__).with_name(file))
    result=importlib.util.module_from_spec(spec);spec.loader.exec_module(result)
    return result

visual=module('glass_visual_test','native-ime-visual.py')
pilot=module('glass_search_test','native-block-search-observer-pilot.py')
recovery=module('glass_recovery_test','native-session-recovery.py')


class GlassProfileTests(unittest.TestCase):
    owned = dict(x=24, y=24, width=1612, height=976)

    def frame(self, mode='block'):
        image = Image.new('RGB', (1660, 1024), (17, 33, 71))
        r = g.mode_slot(self.owned, mode)
        draw=ImageDraw.Draw(image)
        draw.rounded_rectangle((r['x'],r['y'],r['x']+r['width']-1,r['y']+r['height']-1), radius=6, fill=(62,108,255))
        draw.text((r['x']+12,r['y']+10),mode.title(),fill=(239,245,255))
        return image

    def test_reference_domains_use_full_input_and_exclude_actual_control_row(self):
        p = g.layout(dict(x=0,y=0,width=1612,height=976))
        self.assertEqual(p['header'], dict(x=0,y=0,width=1612,height=42))
        self.assertEqual(p['center'], dict(x=274,y=84,width=1040,height=886))
        self.assertEqual(p['terminal'], dict(x=282,y=90,width=1024,height=830))
        self.assertEqual(p['native_body'], dict(x=282,y=90,width=1024,height=842))
        self.assertEqual(p['native_body']['y']+p['native_body']['height'],932)

    def test_compact_domains_reclaim_native_rows_and_columns_at_the_same_cell_size(self):
        current = g.layout(dict(x=0,y=0,width=1612,height=976))['terminal']
        previous = dict(width=859,height=648)
        # Compare real observation space at unchanged 8px columns/18px rows.
        self.assertGreaterEqual(current['width']//8 - previous['width']//8, 20)
        self.assertGreaterEqual(current['height']//18 - previous['height']//18, 10)

    def test_compact_mode_slots_keep_touch_targets_inside_the_topbar(self):
        for width in (700, 959, 960, 1179, 1180, 1279, 1280, 1612):
            owned = dict(x=24,y=25,width=width,height=704)
            topbar = g.layout(owned)['header']
            block, chat = [g.mode_slot(owned, mode) for mode in ('block', 'chat')]
            for slot in (block, chat):
                self.assertTrue(g.contained(slot, topbar))
                self.assertEqual((slot['width'], slot['height']), (64, 32))
            self.assertEqual(chat['x'] - block['x'] - block['width'], 6)

    def test_offset_short_narrow_and_threshold_are_exact(self):
        for width,height in [(700,640),(1179,704),(1180,704),(1280,875)]:
            plain=g.layout(dict(x=0,y=0,width=width,height=height))
            offset=g.layout(dict(x=24,y=25,width=width,height=height))
            for name in ('header','center','native_body','terminal'):
                self.assertEqual(offset[name], dict(plain[name],x=plain[name]['x']+24,y=plain[name]['y']+25))
        self.assertEqual(g.layout(dict(x=0,y=0,width=1179,height=704))['center']['width'],901)
        self.assertEqual(g.layout(dict(x=0,y=0,width=1180,height=704))['center']['width'],608)
        for bad in [dict(x=0,y=0,width=699,height=640),dict(x=0,y=0,width=1280.5,height=900),dict(x=-1,y=0,width=1280,height=900)]:
            with self.assertRaises(RuntimeError):g.layout(bad)

    def test_selected_mode_is_real_paint_independent_of_background(self):
        for mode in ('block','chat'):
            image=self.frame(mode)
            self.assertEqual(g.header_paint(image,self.owned,mode)['selected_mode'],mode)
            self.assertEqual(g.header_paint(image,self.owned)['selected_mode'],mode)

    def test_wrong_mode_missing_duplicate_old_palette_and_one_pixel_cannot_witness(self):
        with self.assertRaises(RuntimeError):g.header_paint(self.frame('chat'),self.owned,'block')
        for case in ('missing','duplicate','old','one-pixel','foreign','partial'):
            image=self.frame()
            draw=ImageDraw.Draw(image)
            r=g.mode_slot(self.owned,'block')
            if case in ('missing','old','one-pixel','partial'):
                draw.rectangle((r['x'],r['y'],r['x']+r['width']-1,r['y']+r['height']-1),fill=(15,23,38) if case=='old' else (0,0,0))
            if case=='one-pixel':draw.point((r['x']+5,r['y']+5),fill=(62,108,255))
            if case=='partial':draw.rectangle((r['x'],r['y'],r['x']+20,r['y']+40),fill=(62,108,255))
            if case=='duplicate':
                c=g.mode_slot(self.owned,'chat');draw.rounded_rectangle((c['x'],c['y'],c['x']+c['width']-1,c['y']+c['height']-1),radius=6,fill=(62,108,255))
            if case=='foreign':
                draw.rectangle((r['x'],r['y'],r['x']+r['width']-1,r['y']+r['height']-1),fill=(0,0,0));draw.rectangle((2,2,100,50),fill=(62,108,255))
            with self.subTest(case=case),self.assertRaises(RuntimeError):g.header_paint(image,self.owned)

    def test_unknown_profile_and_offscreen_window_reject(self):
        class Smoke:native_ui_profile='unknown'
        with self.assertRaises(RuntimeError):g.selected(Smoke())
        with self.assertRaises(RuntimeError):g.header_paint(Image.new('RGB',(100,100)),self.owned)


class SyntheticSmoke:
    """Socket-free orchestration double, never native input/paint evidence."""
    native_ui_profile='glass-v1'
    backend='wayland'
    recovery_window=5

    def __init__(self,out):
        self.out=Path(out);self.config=self.out/'config.toml'
        self.config.write_text('terminal_mode="block"\nfont_size=14\nui_font_size=14\nline_spacing=1\n')
        self.app=SimpleNamespace(pid=os.getpid(),poll=lambda:None)
        self.transcript=self.out/'sentinels.txt'
        self.transcript.write_text(f'preedit-switch-ready:{os.getpid()}:/dev/pts/99\n')
        self.rect=dict(x=0,y=0,width=1280,height=900)
        self.present=False;self.partial=False;self.only_whole=False;self.shift_body=0
        self.tree_calls=0;self.mutate_after=None;self.focused=True;self.pid=self.app.pid
        self.captures=[]

    def screenshot(self,label):
        self.captures.append(label)
        image=Image.new('RGB',(1280,900),(21,34,61));draw=ImageDraw.Draw(image)
        r=g.mode_slot(self.rect,'block')
        draw.rounded_rectangle((r['x'],r['y'],r['x']+r['width']-1,r['y']+r['height']-1),radius=6,fill=(62,108,255))
        draw.text((r['x']+12,r['y']+10),'Block',fill=(239,245,255))
        if self.present:
            token = self.token_box()
            draw.rectangle((token['x'], token['y'], token['x']+token['width']-1,
                            token['y']+token['height']-1), fill=(180,70,220))
        image.save(self.out/(label+'.png'))

    def token_box(self):
        body = g.layout(self.rect)['native_body']
        return dict(x=380, y=body['y']+body['height']-20, width=180, height=18)

    def run(self,args,**kwargs):
        if args[:3]==['swaymsg','-t','get_tree']:
            self.tree_calls+=1;rect=dict(self.rect)
            if self.mutate_after and self.tree_calls%2==0:rect.update(self.mutate_after)
            node=dict(app_id='io.github.beamiter.ember',pid=self.pid,id=5,focused=self.focused,rect=rect)
            return SimpleNamespace(stdout=json.dumps(dict(nodes=[node])))
        if args[:3]==['swaymsg','-t','get_outputs']:
            return SimpleNamespace(stdout=json.dumps([dict(active=True,scale=1,transform='normal',rect=visual.OUTPUT_RECT)]))
        if args[0]=='identify':
            with Image.open(args[-1]) as image:return SimpleNamespace(stdout=f'{image.width} {image.height}')
        if args[0]=='convert':
            with Image.open(args[1]) as image:
                image=image.convert('RGB')
                if '-crop' in args:
                    w,h,x,y=map(int,re.fullmatch(r'(\d+)x(\d+)\+(\d+)\+(\d+)',args[args.index('-crop')+1]).groups())
                    image=image.crop((x,y,x+w,y+h))
                if args[-1]=='RGB:-':return SimpleNamespace(stdout=image.tobytes())
                image.save(args[-1]);return SimpleNamespace(stdout=b'')
        raise AssertionError(args)

    def words(self,label,**kwargs):
        domains=g.layout(self.rect)
        crop=domains['native_body'] if '-native-body-' in label else domains['center'] if label.endswith('-native-canvas') else dict(x=0,y=0)
        def word(text,x,y,w=180,h=18):
            return dict(level='5',conf='95',text=text,left=str(x-crop['x']),top=str(y-crop['y']),width=str(w),height=str(h))
        rows=[word('status',400,300,w=45,h=14)]
        if self.present and not (self.only_whole and crop['x']):
            text='X'+visual.PREEDIT_TOKEN if self.partial else visual.PREEDIT_TOKEN
            token = self.token_box()
            rows.append(word(text, token['x']+(self.shift_body if '-native-body-' in label else 0), token['y']))
        return rows


class GlassIntegrationTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup)
        self.smoke=SyntheticSmoke(self.temp.name)
        for name in ('REFERENCE_SHELL_HEALTH','REFERENCE_INLINE_BODY'):
            p=patch.object(visual,name,True);p.start();self.addCleanup(p.stop)

    def capture(self,label,present,baseline=None):
        self.smoke.present=present
        return visual.snapshot(self.smoke,label,baseline,present,health_footer=True)

    def test_original_snapshot_lifecycle_keeps_exact_cross_crop_coordinates_and_restore(self):
        before=self.capture('before',False)
        during=self.capture('during',True,before)
        after=self.capture('after',False,before)
        self.assertEqual([item['observed_token_count'] for item in (before,during,after)],[0,1,0])
        token = self.smoke.token_box()
        self.assertEqual(visual.box(during['matches'][0]),token)
        terminal = g.layout(self.smoke.rect)['terminal']
        self.assertGreater(token['y']+token['height'],terminal['y']+terminal['height'])
        self.assertTrue(g.contained(token, g.layout(self.smoke.rect)['native_body']))
        self.assertEqual(during['native_body_regions'][0]['rectangle'],g.layout(self.smoke.rect)['native_body'])
        self.assertEqual(set(during['token_observation_groups'][0]['sources']),{'whole-image-psm6','whole-native-canvas-psm6','during-native-body-0'})
        self.assertTrue(visual.verify_restoration(self.smoke,before,during,after)['after_restored'])
        self.assertEqual(self.smoke.captures,['before','during','after'])

    def test_partial_word_only_whole_and_one_pixel_crop_disagreement_are_rejected(self):
        for field,value,reason in [('partial',True,'Expected 1'),('only_whole',True,'native plane'),('shift_body',1,'geometry')]:
            with self.subTest(field=field):
                setattr(self.smoke,field,value)
                with self.assertRaisesRegex(RuntimeError,reason):self.capture(field,True)
                setattr(self.smoke,field,False)

    def test_header_missing_and_wrong_configured_mode_cannot_satisfy_real_sentinel(self):
        self.smoke.screenshot('health');image=self.smoke.out/'health.png'
        self.smoke.config.write_text('terminal_mode="chat"\n')
        with self.assertRaisesRegex(RuntimeError,'wrong-mode'):
            visual.reference_header_health(self.smoke,'wrong',image,dict(rect=self.smoke.rect))
        self.smoke.config.write_text('terminal_mode="block"\n');Image.new('RGB',(1280,900)).save(image)
        with self.assertRaisesRegex(RuntimeError,'Absent'):
            visual.reference_header_health(self.smoke,'missing',image,dict(rect=self.smoke.rect))

    def test_owned_focus_pid_and_changed_geometry_are_not_replaced_by_paint(self):
        for field,value in [('focused',False),('pid',self.smoke.app.pid+1)]:
            old=getattr(self.smoke,field);setattr(self.smoke,field,value)
            with self.subTest(field=field),self.assertRaisesRegex(RuntimeError,'identity/focus'):g.owned_window(self.smoke)
            setattr(self.smoke,field,old)
        self.smoke.mutate_after=dict(width=1279)
        with self.assertRaisesRegex(RuntimeError,'geometry changed'):self.capture('moved',False)

    def test_search_domain_does_not_follow_target_words_or_alpha_palette(self):
        self.smoke.screenshot('search')
        with Image.open(self.smoke.out/'search.png') as image:
            r=g.layout(self.smoke.rect)['terminal']
            expected=(r['x'],r['y'],r['x']+r['width'],r['y']+r['height'])
            self.assertEqual(pilot.terminal_rectangle(image,(0,0,1280,900),profile='glass-v1'),expected)
            ImageDraw.Draw(image).text((500,500),'QUERY_NEIGHBOR_TARGET',fill=(255,255,255))
            self.assertEqual(pilot.terminal_rectangle(image,(0,0,1280,900),profile='glass-v1'),expected)

    def test_recovery_startup_requires_real_owned_selected_control_paint(self):
        with patch.object(recovery.driver,'wait_for',side_effect=lambda predicate,*a,**k:self.assertTrue(predicate())):
            recovery.wait_startup_surface(self.smoke)
        record=json.loads((self.smoke.out/'startup-paint.json').read_text())
        self.assertEqual(record['observations'][0]['witness']['selected_mode'],'block')
        self.smoke.tree_calls=0;self.smoke.mutate_after=dict(width=1279)
        with patch.object(recovery.driver,'wait_for',side_effect=lambda predicate,*a,**k:predicate()):
            with self.assertRaisesRegex(RuntimeError,'geometry changed'):recovery.wait_startup_surface(self.smoke)


if __name__=='__main__':unittest.main()
