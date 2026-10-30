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

class GlassPopupPlaneTests(unittest.TestCase):
 selector=word('Block',507,490,33,11)
 owned=dict(x=0,y=0,width=1356,height=850)
 left_rgb=(42,64,108)
 right_rgb=(36,51,84)
 def region(self):return d.settings_mode_popup_region(self.selector,self.owned)
 def pixels(self,left_rows=None,right_rows=None,left_rgb=None,right_rgb=None,extra_right=False):
  region=self.region()
  raw=bytearray(bytes((21,39,76))*region['width']*region['height'])
  if left_rows is None:left_rows=[y for y in range(15,77) if y not in (25,26,30,31,47)]
  if right_rows is None:right_rows=[y for y in range(24,77) if y != 47]
  for x,rows,rgb in [(95,left_rows,left_rgb or self.left_rgb),(194,right_rows,right_rgb or self.right_rgb)]:
   for y in rows:
    index=(y*region['width']+x)*3;raw[index:index+3]=bytes(rgb)
  if extra_right:
   for y in right_rows:
    index=(y*region['width']+204)*3;raw[index:index+3]=bytes(right_rgb or self.right_rgb)
  return bytes(raw)
 def observe(self,pixels,profile='glass-v1'):
  return d.settings_mode_popup_plane(pixels,self.region(),self.selector,profile)
 def test_measured_glass_edges_register_with_exact_paired_support(self):
  self.assertEqual(self.region(),dict(x=407,y=501,width=266,height=77))
  self.assertEqual(self.observe(self.pixels()),dict(x=502,y=516,width=100,height=62))
  left={y for y in range(15,77) if y not in (25,26,30,31,47)}
  right={y for y in range(24,77) if y != 47}
  self.assertEqual(len(left & right),48)
 def test_profiles_do_not_admit_each_others_palette(self):
  self.assertIsNone(self.observe(self.pixels(),'legacy-reference'))
  self.assertIsNone(self.observe(self.pixels(left_rgb=(60,60,60),right_rgb=(60,60,60))))
 def test_swapped_sides_and_one_channel_color_changes_are_rejected(self):
  for pixels in [self.pixels(left_rgb=self.right_rgb,right_rgb=self.left_rgb),
                 self.pixels(left_rgb=(41,64,108)),self.pixels(right_rgb=(36,51,83)),
                 self.pixels(left_rgb=(74,99,156),right_rgb=(70,91,140))]:
   self.assertIsNone(self.observe(pixels))
 def test_insufficient_exact_row_support_cannot_register(self):
  self.assertIsNone(self.observe(self.pixels(left_rows=range(20,41),right_rows=range(20,41))))
 def test_individually_supported_edges_need_enough_paired_rows(self):
  self.assertIsNone(self.observe(self.pixels(left_rows=range(10,35),right_rows=range(30,60))))
 def test_three_missing_raster_rows_fail_without_fragment_search(self):
  rows=[y for y in range(15,77) if y not in (40,41,42)]
  self.assertIsNone(self.observe(self.pixels(left_rows=rows,right_rows=rows)))
 def test_ambiguous_glass_planes_are_rejected(self):
  with self.assertRaisesRegex(RuntimeError,'Ambiguous'):
   self.observe(self.pixels(extra_right=True))
 def test_unknown_profile_does_not_fall_back(self):
  with self.assertRaisesRegex(RuntimeError,'Unknown'):
   self.observe(self.pixels(),'glass-next')
 def test_glass_profile_provenance_retains_actual_immutable_capture(self):
  metadata=d.settings_popup_profile_metadata('glass-v1')
  self.assertEqual(metadata['profile'],'glass-v1')
  self.assertEqual(metadata['left_border_rgb'],[42,64,108])
  self.assertEqual(metadata['right_border_rgb'],[36,51,84])
  self.assertEqual(metadata['minimum_paired_exact_rows'],'2 * selector height')
  self.assertEqual(metadata['maximum_missing_raster_rows'],2)
  self.assertEqual(metadata['calibration']['source_image_sha256'],
                   '3b3e31d9d60d6f3e7ad8b736c7c5ccc35e04f76902a1db3e637ae4a8b13edef2')
  self.assertEqual(metadata['calibration']['source_binary_sha256'],
                   '5a4235d633bc0cd94700e5fe6b3cbf1f431d7dbf412a0580ff98380387de6634')
  self.assertEqual(metadata['calibration']['selector_source_sha256'],
                   'bfc467c108852a24d6b8cb10e3afa06f3a99ab86a26ddedc733e7de2f7ad5282')
  self.assertEqual(metadata['calibration']['selector_observer'],'strict native OCR selector')
  self.assertEqual(metadata['calibration']['paired_exact_rows'],48)
  self.assertEqual(metadata['calibration']['measured_plane'],dict(x=502,y=516,width=100,height=62))
  self.assertEqual((metadata['calibration']['backend'],metadata['calibration']['renderer'],
                    metadata['calibration']['scale']),('x11','glow',1))

class SettingsTabAnchorTests(unittest.TestCase):
 def rows(self, font='Font'):
  return [word(font,360,163,44,37),word('Appearance',430,176,80,12),
          word('Advanced',525,176,70,12),word('Advanced',360,226,70,12)]
 def test_selected_font_framing_keeps_unique_same_row_anchor(self):
  for font in ['Font','(Font)','[Font]','‘Font’']:
   rows=self.rows(font)
   self.assertIs(d.settings_advanced_tab(rows,reference_shell_fixture=True),rows[2])
 def test_unknown_framing_spelling_or_case_does_not_become_font(self):
  for font in ['*Font*','F0nt','font','Font settings']:
   with self.assertRaisesRegex(AssertionError,'Font tab-row'):
    d.settings_advanced_tab(self.rows(font),reference_shell_fixture=True)
 def test_font_from_another_row_cannot_anchor_advanced(self):
  rows=self.rows();rows[0]=word('(Font)',360,300,44,12)
  with self.assertRaisesRegex(AssertionError,'Font tab-row'):
   d.settings_advanced_tab(rows,reference_shell_fixture=True)
 def test_duplicate_framed_and_unframed_font_anchors_are_rejected(self):
  rows=self.rows();rows.append(word('(Font)',365,176,44,12))
  with self.assertRaisesRegex(AssertionError,'Font tab-row'):
   d.settings_advanced_tab(rows,reference_shell_fixture=True)
 def test_duplicate_advanced_tab_candidates_are_rejected(self):
  rows=self.rows();rows.append(word('(Advanced)',600,176,78,12))
  with self.assertRaisesRegex(AssertionError,'Advanced tab'):
   d.settings_advanced_tab(rows,reference_shell_fixture=True)

if __name__=='__main__':unittest.main()
