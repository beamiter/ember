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

 def authoritative(self,rows,region=None):
  return d.registered_settings_popup_rows(rows,region or dict(x=489,y=584,width=100,height=65))
 def test_registered_pass_is_authoritative_without_cross_pass_box_tolerance(self):
  # The exact CI image produced these two boxes for one physical Chat item.
  # The original union remains a failure; the predeclared one-pass observer
  # receives neither a competing whole-image vote nor any repaired word box.
  selector=word('Block',479,522,37,11)
  whole=word('Chat',486,590,32,11);tight=word('Chat',485,590,33,11)
  with self.assertRaisesRegex(RuntimeError,'popup item'):
   d.settings_mode_popup(d.merge_settings_ocr_rows([whole],[tight]),selector,'chat')
  observed=d.registered_settings_popup_rows([tight],dict(x=474,y=548,width=100,height=62))
  self.assertIs(d.settings_mode_popup(observed,selector,'chat'),tight)
 def test_authoritative_pass_rejects_same_pass_duplicate(self):
  with self.assertRaisesRegex(RuntimeError,'Duplicate'):
   self.authoritative([word('Block'),word('Block')])
 def test_authoritative_pass_keeps_one_pixel_different_same_pass_boxes_ambiguous(self):
  rows=self.authoritative([word('Block'),word('Block',501,592)])
  with self.assertRaisesRegex(RuntimeError,'popup item'):
   d.settings_mode_popup(rows,self.selector,'block')
 def test_authoritative_pass_keeps_two_actual_same_named_items_ambiguous(self):
  rows=self.authoritative([word('Block'),word('Block',500,615)])
  with self.assertRaisesRegex(RuntimeError,'popup item'):
   d.settings_mode_popup(rows,self.selector,'block')
 def test_authoritative_two_different_items_select_only_exact_requested_name(self):
  first,second=word('Block'),word('Chat',500,615)
  observed=self.authoritative([first,second])
  self.assertIs(d.settings_mode_popup(observed,self.selector,'block'),first)
  self.assertIs(d.settings_mode_popup(observed,self.selector,'chat'),second)
 def test_authoritative_pass_cannot_use_words_outside_registered_popup(self):
  for row in [word('Block',488,592),word('Block',500,583),word('Block',570,592),word('Block',500,645)]:
   with self.assertRaisesRegex(RuntimeError,'registered crop'):
    self.authoritative([row])
 def test_authoritative_pass_requires_registered_nonzero_geometry(self):
  for region in [None,dict(x=489,y=584,width=0,height=65),dict(x=-1,y=584,width=100,height=65)]:
   with self.assertRaisesRegex(RuntimeError,'registered native'):
    d.registered_settings_popup_rows([word('Block')],region)
 def test_empty_or_fuzzy_registered_pass_cannot_select_a_mode(self):
  for rows in [[],[word('block')],[word('Block.')]]:
   with self.assertRaisesRegex(RuntimeError,'popup item'):
    d.settings_mode_popup(self.authoritative(rows),self.selector,'block')

if __name__=='__main__':unittest.main()
