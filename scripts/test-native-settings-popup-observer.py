"""Exact native Settings popup-plane and immutable-ROI targeting negatives."""
import importlib.util
from pathlib import Path
import unittest
spec=importlib.util.spec_from_file_location('popup_desktop',Path(__file__).with_name('native-desktop-smoke.py'))
d=importlib.util.module_from_spec(spec);spec.loader.exec_module(d)

def word(text,x=500,y=592,width=43,height=12):
 return dict(text=text,left=str(x),top=str(y),width=str(width),height=str(height))

class PopupTests(unittest.TestCase):
 selector=word('Chat',494,553,36,12)
 owned=dict(x=0,y=25,width=1280,height=875)
 def region(self):return d.settings_mode_popup_region(self.selector,self.owned)
 def pixels(self,borders=(),color=(60,60,60)):
  region=self.region();raw=bytearray(bytes((29,29,29))*region['width']*region['height'])
  for x,start,end in borders:
   for y in range(start,end):
    index=(y*region['width']+x)*3;raw[index:index+3]=bytes(color)
  return bytes(raw)
 def test_region_keeps_original_selector_admissible_geometry(self):
  self.assertEqual(self.region(),dict(x=394,y=565,width=272,height=84))
 def test_independent_border_plane_measures_native_popup(self):
  self.assertEqual(d.settings_mode_popup_plane(self.pixels([(95,19,84),(194,19,84)]),self.region(),self.selector),dict(x=489,y=584,width=100,height=65))
 def test_absent_or_short_or_wrong_palette_border_does_not_register(self):
  for pixels in [self.pixels(),self.pixels([(95,19,30),(194,19,30)]),self.pixels([(95,19,84),(194,19,84)],(59,59,59))]:
   self.assertIsNone(d.settings_mode_popup_plane(pixels,self.region(),self.selector))
 def test_ambiguous_planes_fail_instead_of_selecting_first(self):
  with self.assertRaisesRegex(RuntimeError,'Ambiguous'):
   d.settings_mode_popup_plane(self.pixels([(95,19,84),(194,19,84),(204,19,84)]),self.region(),self.selector)
 def test_wrong_rgb_length_fails_closed(self):
  with self.assertRaisesRegex(RuntimeError,'RGB size'):d.settings_mode_popup_plane(b'',self.region(),self.selector)
 def test_region_cannot_cross_owned_surface(self):
  for selector in [word('Chat',10,553,36,12),word('Chat',494,850,36,12),word('Chat',494,553,0,12)]:
   with self.assertRaises(RuntimeError):d.settings_mode_popup_region(selector,self.owned)
 def test_toolbar_only_and_selector_without_popup_do_not_satisfy_menu(self):
  for rows in [[word('Block',1150,41,29,9)],[self.selector],[word('Block',494,553)]]:
   with self.assertRaisesRegex(RuntimeError,'popup item'):d.settings_mode_popup(rows,self.selector,'block')
 def test_exact_menu_word_survives_toolbar_and_old_selector(self):
  target=word('Block');rows=d.merge_settings_ocr_rows([word('Block',1150,41,29,9),self.selector],[target])
  self.assertIs(d.settings_mode_popup(rows,self.selector,'block'),target)
 def test_two_same_named_menu_items_still_fail_unique_target(self):
  rows=d.merge_settings_ocr_rows([], [word('Block'),word('Block',500,615)])
  with self.assertRaisesRegex(RuntimeError,'popup item'):d.settings_mode_popup(rows,self.selector,'block')
 def test_duplicate_rows_in_one_crop_fail_even_at_same_box(self):
  with self.assertRaisesRegex(RuntimeError,'Duplicate'):d.merge_settings_ocr_rows([], [word('Block'),word('Block')])
 def test_only_exact_same_cross_pass_text_box_deduplicates(self):
  self.assertEqual(len(d.merge_settings_ocr_rows([word('Block')],[word('Block')])),1)
  rows=d.merge_settings_ocr_rows([word('Block')],[word('Block',501,592)])
  with self.assertRaisesRegex(RuntimeError,'popup item'):d.settings_mode_popup(rows,self.selector,'block')
 def test_fuzzy_or_punctuation_word_does_not_match(self):
  for text in ['block','Bloc!','(Block','Block.']:
   with self.assertRaisesRegex(RuntimeError,'popup item'):d.settings_mode_popup([word(text)],self.selector,'block')

if __name__=='__main__':unittest.main()
