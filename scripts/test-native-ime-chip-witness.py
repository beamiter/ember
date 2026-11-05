"""Exact-paint witnesses only; no compositor, engine or native input is run."""
import importlib.util
from pathlib import Path
import unittest
from PIL import Image, ImageDraw

spec = importlib.util.spec_from_file_location('visual', Path(__file__).with_name('native-ime-visual.py'))
visual = importlib.util.module_from_spec(spec); spec.loader.exec_module(visual)


class ChipTests(unittest.TestCase):
    body = dict(x=10, y=10, width=380, height=150)
    contract = dict(min_height=16, line_height=23)

    def fixture(self, fill=(231, 237, 253), ink=(40, 52, 76), region=(30, 30, 230, 50)):
        image = Image.new('RGB', (400, 180), (5, 12, 24))
        self.paint(image, region, fill, ink)
        return image

    def paint(self, image, region, fill=(231, 237, 253), ink=(40, 52, 76)):
        draw = ImageDraw.Draw(image)
        draw.rounded_rectangle(region, radius=3, fill=fill)
        for x in range(region[0]+10, region[2]-10, 8):
            draw.rectangle((x, region[1]+5, x+2, region[3]-5), fill=ink)

    def planes(self, image, body=None):
        return visual.preedit_chip_planes(image.tobytes(), *image.size, body or self.body, self.contract)

    def token(self, x=50, y=34, w=160, h=12):
        return dict(left=str(x), top=str(y), width=str(w), height=str(h))

    def test_paint_is_registered_before_and_without_token_coordinates(self):
        planes = self.planes(self.fixture())
        self.assertEqual(len(planes), 1)
        self.assertEqual(planes[0]['rectangle'], dict(x=30,y=30,width=201,height=21))
        self.assertEqual(planes[0]['profile'], 'production-preedit-chip-v1')
        self.assertGreater(planes[0]['ink_pixels'], 0)
        self.assertEqual(visual.require_native_plane_witness(self.token(), {'rect': self.body}, self.body, planes), planes[0])

    def test_wrong_palette_uniform_fill_low_density_or_wrong_line_height_fail(self):
        images = [self.fixture(fill=(230,237,253)), self.fixture(ink=(41,52,76)),
                  self.fixture(ink=(231,237,253)), self.fixture(region=(30,30,230,60)),
                  self.fixture(region=(30,30,230,40)), self.fixture(region=(30,30,55,50))]
        dense = self.fixture(); ImageDraw.Draw(dense).rectangle((35,32,225,48), fill=(40,52,76));images.append(dense)
        for image in images:
            with self.subTest(image=images.index(image)):
                self.assertEqual(self.planes(image), [])

    def test_missing_paint_unowned_chip_and_cross_chip_word_fail(self):
        self.assertEqual(self.planes(Image.new('RGB',(400,180),(5,12,24))), [])
        self.assertEqual(self.planes(self.fixture(), dict(x=40,y=10,width=350,height=150)), [])
        planes = self.planes(self.fixture())
        for word in [self.token(x=20), self.token(w=190), self.token(y=29), self.token(h=20)]:
            with self.assertRaises(RuntimeError):
                visual.require_native_plane_witness(word, {'rect':self.body}, self.body, planes)
        with self.assertRaises(RuntimeError):
            visual.require_native_plane_witness(self.token(), {'rect':self.body}, self.body, planes*2)

    def test_all_components_are_kept_and_never_selected_by_target_text(self):
        image = self.fixture(); self.paint(image,(30,80,230,100))
        planes = self.planes(image)
        self.assertEqual(len(planes),2)
        self.assertEqual({p['rectangle']['y'] for p in planes}, {30,80})
        with self.assertRaises(RuntimeError):
            visual.preedit_chip_planes(b'bad',400,180,self.body,self.contract)

    def test_single_preselected_chip_pass_owns_boxes_and_exact_spelling(self):
        planes = self.planes(self.fixture())
        token = dict(self.token(),text=visual.PREEDIT_TOKEN)
        auth = [('chip-psm7',[token])]
        # Full-image segmentation differs but cannot add another occurrence.
        diagnostic = [('whole',[dict(token,width='159')]),('body',[])]
        self.assertEqual(visual.require_chip_observation_agreement(auth,diagnostic,{'rect':self.body},planes),[token])
        for wrong in [visual.PREEDIT_TOKEN.lower(),'_'+visual.PREEDIT_TOKEN,visual.PREEDIT_TOKEN+'.']:
            with self.assertRaises(RuntimeError):
                visual.require_chip_observation_agreement([('chip',[dict(token,text=wrong)])],diagnostic,{'rect':self.body},planes)

    def test_duplicates_and_outside_or_unpainted_exact_tokens_still_fail(self):
        planes = self.planes(self.fixture())
        token = dict(self.token(),text=visual.PREEDIT_TOKEN)
        auth = [('chip',[token])]
        for diagnostic in [[('whole',[token,token])], [('whole',[dict(token,left='15')])],
                           [('whole',[dict(token,top='75')])]]:
            with self.assertRaises(RuntimeError):
                visual.require_chip_observation_agreement(auth,diagnostic,{'rect':self.body},planes)
        with self.assertRaises(RuntimeError):
            visual.require_chip_observation_agreement([('chip',[token,token])],[],{'rect':self.body},planes)
        with self.assertRaises(RuntimeError):
            visual.require_chip_observation_agreement(auth,[],{'rect':self.body},planes*2)
        with self.assertRaises(RuntimeError):
            visual.require_chip_observation_agreement([], [('whole',[token])],{'rect':self.body},[])

    def test_absence_and_multiple_painted_tokens_do_not_collapse(self):
        self.assertEqual(visual.require_chip_observation_agreement([], [('whole',[])],{'rect':self.body},[]),[])
        image=self.fixture();self.paint(image,(30,80,230,100));planes=self.planes(image)
        first=dict(self.token(),text=visual.PREEDIT_TOKEN)
        second=dict(self.token(y=84),text=visual.PREEDIT_TOKEN)
        matches=visual.require_chip_observation_agreement([('chip-0',[first]),('chip-1',[second])],[],{'rect':self.body},planes)
        self.assertEqual(matches,[first,second])


if __name__ == '__main__':
    unittest.main()
