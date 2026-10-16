"""Exact Settings Save OCR on unchanged captured pixels, owned Wayland surface."""
import csv
import io
import json
from pathlib import Path


def owned_rect(tree, pid):
    matches=[]
    def visit(node):
        if node.get('app_id') == 'io.github.beamiter.ember':matches.append(node)
        for child in node.get('nodes', []) + node.get('floating_nodes', []):visit(child)
    visit(tree)
    if len(matches)!=1 or matches[0].get('pid')!=pid or matches[0].get('focused') is not True:
        raise RuntimeError('Settings screenshot ownership/focus mismatch')
    node=matches[0];rect=node['rect']
    if (any(type(rect.get(k)) is not int for k in ('x','y','width','height')) or
            min(rect['width'],rect['height'])<=0 or min(rect['x'],rect['y'])<0 or
            rect['x']+rect['width']>1280 or rect['y']+rect['height']>900):
        raise RuntimeError('Invalid Settings owned rectangle')
    return {'id':node['id'],'pid':pid,'rect':rect}


def exact_save(rows, owned, dimensions):
    matches=[r for r in rows if r['text'].strip()=='Save']
    if len(matches)>1:raise RuntimeError('Ambiguous exact Settings Save')
    if not matches:return None
    row=matches[0];x,y,w,h=(int(row[k]) for k in ('left','top','width','height'));r=owned['rect']
    if (w<=0 or h<=0 or x<0 or y<0 or x+w>dimensions[0] or y+h>dimensions[1] or
            x<r['x'] or y<r['y'] or x+w>r['x']+r['width'] or y+h>r['y']+r['height']):
        raise RuntimeError('Settings Save outside owned screenshot')
    return row


def click_save(smoke, label):
    if smoke.backend!='wayland' or not label.endswith('-mode-save'):
        raise RuntimeError('Save fallback is limited to Wayland Settings mode save')
    def geometry(suffix):
        reply=smoke.run(['swaymsg','-t','get_tree','-r'],capture_output=True,text=True)
        (smoke.out/(label+suffix+'-tree.json')).write_text(reply.stdout)
        return owned_rect(json.loads(reply.stdout),smoke.app.pid)
    def dimensions(path):
        result=smoke.run(['identify','-format','%w %h',str(path)],capture_output=True,text=True).stdout
        if result!='1280 900':raise RuntimeError('Settings Save image dimensions changed')
        return (1280,900)
    owned=geometry('-before')
    rows=smoke.words(label)
    original=smoke.out/(label+'.png');size=dimensions(original)
    if geometry('-after')!=owned:raise RuntimeError('Settings geometry changed across screenshot')
    row=exact_save(rows,owned,size)
    if row is None:
        row=exact_save(smoke.words(label,psm=6,capture=False),owned,size)
    if row is None:
        transformed=smoke.out/(label+'-gray-inverted.png')
        command=['convert',str(original),'-colorspace','Gray','-negate',str(transformed)]
        smoke.run(command)
        if dimensions(transformed)!=size:raise RuntimeError('Transformed Settings image dimensions changed')
        reply=smoke.run(['tesseract',str(transformed),'stdout','--psm','11','tsv'],capture_output=True,text=True)
        (smoke.out/(label+'-gray-inverted.tsv')).write_text(reply.stdout)
        rows=list(csv.DictReader(io.StringIO(reply.stdout),delimiter='\t',quoting=csv.QUOTE_NONE))
        (smoke.out/(label+'-transform.json')).write_text(json.dumps({'algorithm':'ImageMagick -colorspace Gray -negate','command':command,'dimensions':size,'owned':owned},indent=2))
        row=exact_save(rows,owned,size)
    if row is None:raise RuntimeError('Absent exact Settings Save')
    if geometry('-preclick')!=owned:raise RuntimeError('Settings geometry changed before click')
    smoke.click(int(row['left'])+int(row['width'])//2,int(row['top'])+int(row['height'])//2)
