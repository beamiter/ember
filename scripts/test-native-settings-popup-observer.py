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
 def region(self):return d.settings_mode_popup_region(self.selector,self.owned)
 def pixels(self,box=(96,15,194,77),color=(16,24,37),extra=None,hole=None):
  from PIL import Image, ImageDraw
  region=self.region();image=Image.new('RGB',(region['width'],region['height']),(15,24,38));draw=ImageDraw.Draw(image)
  for current in [box,extra]:
   if current:
    x,y,right,bottom=current;draw.rectangle((x,y,right-1,bottom-1),fill=color)
  if hole:
   x,y,right,bottom=hole;draw.rectangle((x,y,right-1,bottom-1),fill=(15,24,38))
  return image.tobytes()
 def observe(self,pixels,profile='glass-v1'):
  return d.settings_mode_popup_plane(pixels,self.region(),self.selector,profile)
 def test_exact_opaque_connected_body_is_registered_independently_of_text(self):
  self.assertEqual(self.region(),dict(x=407,y=501,width=266,height=77))
  self.assertEqual(self.observe(self.pixels()),dict(x=503,y=516,width=98,height=62))
  self.assertEqual(self.observe(self.pixels(hole=(105,30,125,40))),dict(x=503,y=516,width=98,height=62))
 def test_profiles_do_not_admit_each_others_palette(self):
  self.assertIsNone(self.observe(self.pixels(),'legacy-reference'))
  for color in [(29,29,29),(60,60,60),(21,39,76),(42,64,108),(16,24,38)]:
   self.assertIsNone(self.observe(self.pixels(color=color)))
 def test_wrong_exact_width_missing_short_or_unanchored_body_rejects(self):
  for box in [None,(96,15,193,77),(96,15,195,77),(96,15,194,36),(0,15,98,77)]:
   with self.subTest(box=box):self.assertIsNone(self.observe(self.pixels(box=box)))
 def test_exact_body_density_is_required_without_color_tolerance(self):
  self.assertIsNone(self.observe(self.pixels(hole=(100,20,190,55))))
 def test_two_independent_menu_bodies_remain_ambiguous(self):
  with self.assertRaisesRegex(RuntimeError,'Ambiguous'):
   self.observe(self.pixels(box=(96,1,194,24),extra=(96,40,194,63)))
 def test_body_must_belong_below_the_observed_selector(self):
  region=self.region();region['y']-=30
  self.assertIsNone(d.settings_mode_popup_plane(self.pixels(),region,self.selector,'glass-v1'))
 def test_whole_dialog_surface_cannot_stand_in_for_its_menu(self):
  region=self.region()
  self.assertIsNone(self.observe(bytes((16,24,37))*region['width']*region['height']))
 def test_unknown_profile_and_wrong_rgb_size_fail_closed(self):
  with self.assertRaisesRegex(RuntimeError,'Unknown'):self.observe(self.pixels(),'glass-next')
  with self.assertRaisesRegex(RuntimeError,'RGB size'):self.observe(b'')
 def test_current_profile_retains_immutable_cross_backend_provenance(self):
  metadata=d.settings_popup_profile_metadata('glass-v1')
  self.assertEqual(metadata['body_rgb'],[16,24,37])
  self.assertEqual(metadata['body_width'],98)
  self.assertEqual(metadata['minimum_body_density'],0.8)
  calibration=metadata['calibration']
  self.assertEqual(calibration['source_commit'],'913207036767934bbd71594eea5eb8b3e1c49bd7')
  self.assertEqual(calibration['source_image_sha256'],'5cafe3ed3d82991668a4c90acc10901f334ccfc6577936b10aa8c05aab08866a')
  self.assertEqual(calibration['cross_backend_image_sha256'],'6e5b40097f7b028ec128e5683aa2be6b5ac1111ff739572959f1b17412d190b8')
  self.assertEqual(calibration['measured_body'],dict(x=475,y=585,width=98,height=67))

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
