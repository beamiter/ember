#!/usr/bin/env python3
"""Public owned-tree targeting adapter; retains legacy search/result oracles."""
import hashlib
import importlib.util
import json
import time
from pathlib import Path


def load(name, filename):
    spec=importlib.util.spec_from_file_location(name,Path(__file__).with_name(filename))
    module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module);return module


public=load('public_control','native-block-search-atspi-interaction.py')
probe=public.probe


def action_button(tree, expected, name):
    found=public.controls(tree,expected)
    picker=found['editor']['parent']
    choices=[n for n in tree['nodes'] if n['parent']==picker and n['name']==name and n['role']==43]
    if len(choices)!=1:
        raise RuntimeError('Missing or ambiguous picker action: '+name)
    node=choices[0]
    by_path={n['path']:n for n in tree['nodes']}
    if (public.COMPONENT not in node['interfaces'] or not public.state(node,25)
            or not public.state(node,30) or not public.inside(node.get('screen_extents',[]),by_path[picker]['screen_extents'])):
        raise RuntimeError('Picker action not visibly bounded')
    public.click_index(node)
    return node


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
        self.smoke=smoke;self.window=window;self.expected='';self.owner=None;self.serial=0
        self.out=smoke.out/'public-targeting';self.out.mkdir()

    def read(self, label):
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

    def button(self, name, label):
        allowed=('Search this retained output','Stop search','Restart this output','Earlier history')
        if name not in allowed:raise RuntimeError('Unapproved operation in bounded adapter')
        tree,_=self.read(label+'-before')
        result=invoke_button(tree,self.expected,name)
        self.event(event='public-button',label=label,**result)
