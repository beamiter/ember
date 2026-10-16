import importlib.util
import json
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest

spec=importlib.util.spec_from_file_location('save',Path(__file__).with_name('native-settings-save-ocr.py'))
save=importlib.util.module_from_spec(spec);spec.loader.exec_module(save)
ROW={'text':'Save','left':'884','top':'666','width':'28','height':'10'}


class SaveTests(unittest.TestCase):
    def fixture(self,directory,passes,tsv=None,dimensions='1280 900',stale=False):
        calls=[];tree={'id':5,'app_id':'io.github.beamiter.ember','pid':42,'focused':True,'rect':{'x':0,'y':0,'width':1280,'height':900}}
        class Smoke:
            backend='wayland';app=SimpleNamespace(pid=42);out=Path(directory)
            def words(self,label,psm=11,capture=True):calls.append(('words',psm,capture));return passes[psm]
            def click(self,x,y):calls.append(('click',x,y))
            def run(self,args,**kwargs):
                calls.append(tuple(args))
                if args[0]=='swaymsg':
                    if stale and len([x for x in calls if x[0]=='swaymsg'])==3:tree['rect']['x']=1
                    return SimpleNamespace(stdout=json.dumps(tree))
                if args[0]=='identify':return SimpleNamespace(stdout=dimensions if '-gray-inverted.png' in args[-1] else '1280 900')
                if args[0]=='tesseract':return SimpleNamespace(stdout=tsv or 'text\tleft\ttop\twidth\theight\nSave\t884\t666\t28\t10\n')
                return SimpleNamespace(stdout='')
        return Smoke(),calls

    def test_original_exact_match_skips_transform_and_clicks_once(self):
        with tempfile.TemporaryDirectory() as d:
            smoke,calls=self.fixture(d,{11:[ROW],6:[]});save.click_save(smoke,'normal-mode-save')
            self.assertEqual([x for x in calls if x[0]=='click'],[('click',898,671)])
            self.assertFalse(any(x[0]=='convert' for x in calls))

    def test_only_two_zero_matches_allow_same_image_transform(self):
        with tempfile.TemporaryDirectory() as d:
            smoke,calls=self.fixture(d,{11:[],6:[]});save.click_save(smoke,'normal-mode-save')
            self.assertEqual([x for x in calls if x[0]=='words'],[('words',11,True),('words',6,False)])
            self.assertEqual(len([x for x in calls if x[0]=='convert']),1)
            self.assertEqual(len([x for x in calls if x[0]=='click']),1)
            self.assertTrue((Path(d)/'normal-mode-save-transform.json').exists())

    def test_ambiguous_original_or_transformed_token_fails_without_click(self):
        for passes,tsv in [({11:[ROW,ROW],6:[]},None),({11:[],6:[ROW,ROW]},None),
                           ({11:[],6:[]},'text\tleft\ttop\twidth\theight\nSave\t884\t666\t28\t10\nSave\t900\t700\t28\t10\n')]:
            with tempfile.TemporaryDirectory() as d:
                smoke,calls=self.fixture(d,passes,tsv)
                with self.assertRaisesRegex(RuntimeError,'Ambiguous'):save.click_save(smoke,'normal-mode-save')
                self.assertFalse(any(x[0]=='click' for x in calls))

    def test_absent_changed_dimensions_and_stale_geometry_fail(self):
        for kwargs in [{'tsv':'text\tleft\ttop\twidth\theight\nOther\t884\t666\t28\t10\n'}, {'dimensions':'2560 1800'}, {'stale':True}]:
            with tempfile.TemporaryDirectory() as d:
                smoke,calls=self.fixture(d,{11:[],6:[]},**kwargs)
                with self.assertRaises(RuntimeError):save.click_save(smoke,'normal-mode-save')
                self.assertFalse(any(x[0]=='click' for x in calls))

    def test_token_outside_owned_rectangle_is_rejected(self):
        owned={'rect':{'x':0,'y':0,'width':800,'height':900}}
        with self.assertRaisesRegex(RuntimeError,'outside'):save.exact_save([ROW],owned,(1280,900))

    def test_wrong_settings_scope_rejected(self):
        with tempfile.TemporaryDirectory() as d:
            smoke,calls=self.fixture(d,{11:[ROW],6:[]})
            with self.assertRaises(RuntimeError):save.click_save(smoke,'arbitrary')
            self.assertEqual(calls,[])


if __name__=='__main__':unittest.main()
