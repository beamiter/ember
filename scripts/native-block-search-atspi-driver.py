#!/usr/bin/env python3
"""Public owned-tree targeting adapter; retains legacy search/result oracles."""
import hashlib
import importlib.util
import json
import re
import time
from pathlib import Path


def load(name, filename):
    spec=importlib.util.spec_from_file_location(name,Path(__file__).with_name(filename))
    module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module);return module


public=load('public_control','native-block-search-atspi-interaction.py')
probe=public.probe


class MissingTarget(RuntimeError):
    pass


def action_button(tree, expected, name):
    found=public.controls(tree,expected)
    picker=found['editor']['parent']
    choices=[n for n in tree['nodes'] if n['parent']==picker and n['name']==name and n['role']==43]
    if not choices:raise MissingTarget('Missing picker action: '+name)
    if len(choices)!=1:
        raise RuntimeError('Ambiguous picker action: '+name)
    node=choices[0]
    by_path={n['path']:n for n in tree['nodes']}
    if (public.COMPONENT not in node['interfaces'] or not public.state(node,25)
            or not public.state(node,30) or not public.inside(node.get('screen_extents',[]),by_path[picker]['screen_extents'])):
        raise RuntimeError('Picker action not visibly bounded')
    public.click_index(node)
    return node


def snapshot_hit(tree, expected, line):
    """Exact public result label, never the query field or a substring hit."""
    found=public.controls(tree,expected)
    nodes={n['path']:n for n in tree['nodes']}
    picker=nodes[found['editor']['parent']]
    if not public.selected(found,'Out'):raise RuntimeError('Result requires verified Out scope')
    pattern=r'Result ([1-9][0-9]*) of ([1-9][0-9]*); '+re.escape(line)+r'; output line [0-9]+ for .+'
    matches=[n for n in tree['nodes'] if n['role']==62 and re.fullmatch(pattern,n['name'])]
    if not matches:raise MissingTarget('Missing exact snapshot result label')
    if len(matches)!=1:raise RuntimeError('Ambiguous exact snapshot result label')
    node=matches[0]
    match=re.fullmatch(pattern,node['name'])
    if int(match[1])>int(match[2]):raise RuntimeError('Invalid result ordinal')
    ancestor=node
    while ancestor['path']!=picker['path']:
        if ancestor['parent'] not in nodes:raise RuntimeError('Result outside picker')
        ancestor=nodes[ancestor['parent']]
    if (public.COMPONENT not in node['interfaces'] or not public.state(node,25)
            or not public.state(node,30)
            or not public.inside(node.get('screen_extents',[]),picker['screen_extents'])):
        raise RuntimeError('Result not visibly bounded')
    if not public.state(node,public.PRESSED):raise RuntimeError('Result is not the selected toggle')
    public.click_index(node)
    return node


def raw_hit(tree, expected, phase):
    """One fresh tree must co-observe exact retained-row identity and phase."""
    found=public.controls(tree,expected);nodes={n['path']:n for n in tree['nodes']}
    picker=nodes[found['editor']['parent']]
    if not public.selected(found,'Out'):raise RuntimeError('Result requires verified Out scope')
    pattern=r'Result ([1-9][0-9]*) of ([1-9][0-9]*); retained output; Retained row #([0-9]+) · literal match: '+re.escape(expected)
    matches=[n for n in tree['nodes'] if n['role']==62 and re.fullmatch(pattern,n['name'])]
    if not matches:raise MissingTarget('Missing exact raw result')
    if len(matches)!=1:raise RuntimeError('Ambiguous exact raw result')
    node=matches[0];match=re.fullmatch(pattern,node['name'])
    if (node['parent']!=picker['path'] or int(match[1])>int(match[2])
            or public.COMPONENT not in node['interfaces']
            or not all(public.state(node,b) for b in (20,25,30))
            or not public.inside(node.get('screen_extents',[]),picker['screen_extents'])):
        raise RuntimeError('Raw result is not selected/visible in owned picker')
    public.click_index(node)
    forbidden={'active':('Continue this output','Restart this output'),
               'stopped':('Stop search','Continue this output'),
               'complete':('Stop search','Continue this output','Restart this output')}
    if phase not in forbidden:raise RuntimeError('Unsupported raw phase')
    if any(n['name'] in forbidden[phase] or (phase!='complete' and
           'Complete for this retained output only' in n['name']) for n in tree['nodes']):
        raise RuntimeError('Raw phase conflicts with other operation state')
    if phase in ('active','stopped'):
        control=action_button(tree,expected,'Stop search' if phase=='active' else 'Restart this output')
    elif phase=='complete':
        choices=[n for n in tree['nodes'] if n['parent']==picker['path'] and n['role']==29
                 and public.TEXT in n['interfaces'] and n.get('text')==n['name']
                 and re.fullmatch(r'Retained blocks #[0-9]+–#[0-9]+ · Block #[0-9]+: Complete for this retained output only; [0-9]+ chunks this window; [0-9]+ windows, [0-9]+ row decodes, [0-9]+ charged cells, [0-9]+\.[0-9]s',n['name'])]
        if not choices:raise MissingTarget('Missing exact completion status')
        if len(choices)!=1:raise RuntimeError('Ambiguous completion status')
        control=choices[0]
        if (not all(public.state(control,b) for b in (25,30))
                or not public.inside(control.get('screen_extents',[]),picker['screen_extents'])):
            raise RuntimeError('Completion status not visibly bounded')
    else:raise RuntimeError('Unsupported raw phase')
    if phase!='active' and any(n['name']=='Stop search' for n in tree['nodes']):
        raise RuntimeError('Raw activation while loading')
    return {'hit':node,'phase_control':control,
            'claim':{'token':expected,'retained_row':int(match[3]),'result_path':node['path']}}


def evidence_identity(tree, expected, node):
    found=public.controls(tree,expected)
    nodes={n['path']:n for n in tree['nodes']}
    picker=nodes[found['editor']['parent']];window=nodes[picker['parent']]
    return {'controls':public.control_identity(tree,found), 'window':window,
            'picker_bounds':picker['screen_extents'], 'node':node}


def invoke_button(tree, expected, name):
    import dbus
    node=action_button(tree,expected,name)
    session=dbus.SessionBus()
    gateway=dbus.Interface(session.get_object('org.a11y.Bus','/org/a11y/bus'),'org.a11y.Bus')
    bus=dbus.bus.BusConnection(str(gateway.GetAddress(timeout=3)))
    broker=dbus.Interface(bus.get_object('org.freedesktop.DBus','/org/freedesktop/DBus'),'org.freedesktop.DBus')
    def owned():
        if int(broker.GetConnectionUnixProcessID(tree['owner'],timeout=3))!=tree['pid']:
            raise RuntimeError('Application ownership changed around action')
    owned()
    action=dbus.Interface(bus.get_object(tree['owner'],node['path']),public.ACTION)
    observed=[list(map(str,a)) for a in action.GetActions(timeout=3)]
    if observed!=node['actions']:raise RuntimeError('Action list changed')
    index=public.click_index(node)
    if not bool(action.DoAction(index,timeout=3)):raise RuntimeError('Public action rejected: '+name)
    owned()
    return dict(path=node['path'],name=name,index=index,returned=True,response='pending existing search oracle')


class Driver:
    def __init__(self, smoke, window):
        self.smoke=smoke;self.window=window;self.expected='';self.owner=None;self.serial=0;self.raw_deadline=None
        self.out=smoke.out/'public-targeting';self.out.mkdir()

    def read(self, label):
        if self.smoke.app.poll() is not None:raise RuntimeError('Owned application exited')
        if self.smoke.app_window()!=self.window:raise RuntimeError('Owned X11 window changed')
        self.serial+=1
        tree=probe.discovery(self.smoke.app.pid,self.out/f'{self.serial:04d}-{label}.json',self.expected)
        if self.owner is None:self.owner=tree['owner']
        if tree['owner']!=self.owner:raise RuntimeError('Application bus owner changed')
        return tree,public.controls(tree,self.expected)

    def event(self, **entry):
        self.smoke.events.append(dict(monotonic_ns=time.monotonic_ns(),**entry))

    def transition(self, target, operation, before_tree, before, predicate, label):
        identity=public.control_identity(before_tree,before)
        operation_result=public.mutation(before_tree,self.expected,target,operation)
        self.event(event='public-operation',label=label,**operation_result)
        deadline=time.monotonic()+8
        while time.monotonic()<deadline:
            tree,found=self.read(label+'-after')
            if time.monotonic() >= deadline:
                raise RuntimeError('Public response deadline exhausted: '+label)
            if public.control_identity(tree,found)!=identity:raise RuntimeError('Target identity changed during '+label)
            if predicate(found):
                self.event(event='public-response',label=label,response_verified=True)
                return
            time.sleep(.1)
        raise RuntimeError('Public response unproven: '+label)

    def focus_query(self):
        tree,found=self.read('focus-query-before')
        if public.focus_is(found,'editor'):
            self.event(event='verified-existing-state',state='query-focused');return
        # Require exactly one known scope as opposing source; do not infer focus.
        source=[k for k in ('All','Cmd','Out') if public.focus_is(found,k)]
        if len(source)!=1:raise RuntimeError('No proven opposing focus source')
        selection=[k for k in ('All','Cmd','Out') if public.selected(found,k)]
        if len(selection)!=1:raise RuntimeError('Ambiguous scope before focus')
        self.transition('editor','focus',tree,found,
                        lambda f:public.focus_is(f,'editor') and public.selected(f,selection[0]),'query-focus')

    def output_scope(self):
        tree,found=self.read('out-scope-before')
        if public.selected(found,'Out'):
            self.event(event='verified-existing-state',state='scope-Out');return
        if not (public.selected(found,'All') or public.selected(found,'Cmd')):
            raise RuntimeError('No proven opposing scope state')
        self.transition('Out','click',tree,found,lambda f:public.selected(f,'Out'),'scope-Out')

    def query(self, value):
        # open() preserves prior intent, so initial expectation is empty and
        # subsequent opens must still carry our last verified query.
        self.focus_query()
        self.smoke.key('a',('ctrl',));self.smoke.text(value)
        self.smoke.key('a',('ctrl',))
        self.smoke.own_clipboard('E03OWNEDDECOY',f'public-query-{self.serial}')
        self.smoke.key('c',('ctrl',))
        deadline=time.monotonic()+8;actual=b'';expired=False
        while time.monotonic()<deadline:
            actual=self.smoke.read_clipboard()
            expired=time.monotonic() >= deadline
            if expired or actual==value.encode():break
            time.sleep(.1)
        label=f'{self.serial:04d}-clipboard'
        (self.out/(label+'.bin')).write_bytes(actual)
        record=dict(expected_query=value,actual_hex=actual.hex(),sha256=hashlib.sha256(actual).hexdigest())
        (self.out/(label+'.json')).write_text(json.dumps(record,indent=2))
        if actual!=value.encode():raise RuntimeError('Exact owned query clipboard mismatch')
        if expired:raise RuntimeError('Exact owned query clipboard deadline exhausted')
        self.smoke.key('Right');self.expected=value
        tree,found=self.read('query-text-readback')
        if not public.focus_is(found,'editor'):raise RuntimeError('Editor focus lost after query readback')
        record.update(editor_path=tree['editor_path'],actual_text=found['editor']['text'],tree_file=f'{self.serial:04d}-query-text-readback.json')
        (self.out/(label+'.json')).write_text(json.dumps(record,indent=2))
        self.smoke.screenshot('public-query-'+str(self.serial))

    def evidence(self, select, label, timeout=8):
        """Fresh public tree / immutable pixels / public tree bracket."""
        deadline=time.monotonic()+timeout
        tree,_=self.read(label+'-evidence-before')
        node=select(tree)
        before=evidence_identity(tree,self.expected,node)
        image_label=f'public-evidence-{self.serial:04d}-{label}'
        image=self.smoke.out/(image_label+'.png')
        if image.exists():raise RuntimeError('Refusing to reuse prior screenshot')
        self.smoke.screenshot(image_label)
        digest=hashlib.sha256(image.read_bytes()).hexdigest()
        dimensions=self.smoke.run(['identify','-format','%w %h',str(image)],capture_output=True,text=True).stdout
        if not re.fullmatch(r'[1-9][0-9]* [1-9][0-9]*',dimensions):
            raise RuntimeError('Invalid screenshot dimensions')
        width,height=map(int,dimensions.split())
        if not public.inside(before['window']['screen_extents'],[0,0,width,height]):
            raise RuntimeError('Owned window outside screenshot')
        after_tree,_=self.read(label+'-evidence-after')
        try:
            after_node=select(after_tree)
        except MissingTarget as error:
            raise RuntimeError('Public target disappeared across screenshot') from error
        if evidence_identity(after_tree,self.expected,after_node)!=before:
            raise RuntimeError('Public evidence changed across screenshot')
        if hashlib.sha256(image.read_bytes()).hexdigest()!=digest:
            raise RuntimeError('Screenshot changed during public evidence')
        if time.monotonic()>=deadline:raise RuntimeError('Public evidence deadline exhausted')
        record=dict(label=label,pid=tree['pid'],owner=tree['owner'],window=self.window,
                    image=str(image),image_sha256=digest,dimensions=[width,height],
                    before=before,after_verified=True,oracle='exact public label; pixels retained separately')
        (self.out/(image_label+'.json')).write_text(json.dumps(record,indent=2))
        self.event(event='public-visible-evidence',label=label,image_sha256=digest)
        return after_tree,after_node

    def wait_evidence(self, select, label, timeout=30):
        deadline=time.monotonic()+timeout
        while time.monotonic()<deadline:
            try:
                return self.evidence(select,label,timeout=max(0,deadline-time.monotonic()))
            except MissingTarget:
                time.sleep(.1)
        raise RuntimeError('Exact public target deadline exhausted: '+label)

    def snapshot_result(self, line, label):
        _,node=self.wait_evidence(lambda t:snapshot_hit(t,self.expected,line),label)
        return node

    def raw_result(self, phase, label):
        timeout=30
        if phase=='active':
            if self.raw_deadline is None:raise RuntimeError('No current raw operation deadline')
            timeout=self.raw_deadline-time.monotonic()
            if timeout<=0:raise RuntimeError('Raw operation 9.5-second budget exhausted')
        def select(tree):
            if phase == 'complete' and any(n['name'] == 'Stop search' for n in tree['nodes']):
                # Completion is a transition: observe a fully validated active
                # operation while it runs, without invoking Stop or restarting.
                raw_hit(tree,self.expected,'active')
                raise MissingTarget('Owned raw operation is still active')
            return raw_hit(tree,self.expected,phase)
        _,result=self.wait_evidence(select,label,timeout=timeout)
        return result

    def button(self, name, label):
        allowed=('Search this retained output','Stop search','Restart this output','Earlier history')
        if name not in allowed:raise RuntimeError('Unapproved operation in bounded adapter')
        tree,_=self.wait_evidence(lambda t:action_button(t,self.expected,name),label)
        if name in ('Search this retained output','Restart this output'):
            self.raw_deadline=time.monotonic()+9.5
        result=invoke_button(tree,self.expected,name)
        self.event(event='public-button',label=label,**result)
