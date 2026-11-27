use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::rc::Rc;

use crate::dom::tree::NodeId;
use rquickjs::function::{Args, Params};
use rquickjs::{Array, Ctx, Exception, FromJs, Function, Object, Persistent, Result, Value};

use crate::protocol::FrameId;

use super::bindings::{host, host_node_id, world};
use super::world::RealmRegistry;

struct Reaction {
    element: Persistent<Object<'static>>,
    callback: Persistent<Function<'static>>,
    arguments: Persistent<Array<'static>>,
    owner: FrameId,
}

#[derive(Default)]
pub(crate) struct CustomElementReactions {
    stack: Vec<Vec<NodeId>>,
    element_reactions: HashMap<NodeId, VecDeque<Reaction>>,
    backup: Vec<NodeId>,
    backup_scheduled: bool,
    collecting: bool,
    hooks: BTreeMap<FrameId, Persistent<Function<'static>>>,
    defined: BTreeSet<FrameId>,
}

impl CustomElementReactions {
    pub(crate) fn forget_frame(&mut self, frame: FrameId) {
        self.hooks.remove(&frame);
        self.defined.remove(&frame);
        self.element_reactions.retain(|_, reactions| {
            reactions.retain(|reaction| reaction.owner != frame);
            !reactions.is_empty()
        });
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }
}

// https://html.spec.whatwg.org/multipage/custom-elements.html#cereactions
#[inline(never)]
pub(crate) fn enter(ctx: &Ctx<'_>) -> Result<()> {
    push(ctx)
}

// https://html.spec.whatwg.org/multipage/custom-elements.html#cereactions
#[inline(never)]
pub(crate) fn finish<'js>(ctx: &Ctx<'js>, result: Result<Value<'js>>) -> Result<Value<'js>> {
    let exception = result
        .as_ref()
        .err()
        .filter(|error| error.is_exception())
        .map(|_| ctx.catch());
    let delivery = pop(ctx);
    if result.is_err() {
        if let Err(error) = delivery {
            host::report_callback_error(ctx, &error);
        }
        return match exception {
            Some(exception) => Err(ctx.throw(exception)),
            None => result,
        };
    }
    delivery?;
    result
}

fn collect(ctx: &Ctx<'_>) -> Result<()> {
    let registry = world(ctx)?.borrow().registry();
    let hooks = {
        let mut registry = registry.borrow_mut();
        if registry.reactions.collecting {
            return Ok(());
        }
        registry.reactions.collecting = true;
        registry
            .reactions
            .hooks
            .iter()
            .filter(|(frame, _)| registry.reactions.defined.contains(frame))
            .map(|(_, hook)| hook.clone())
            .collect::<Vec<_>>()
    };
    let result = (|| {
        for hook in hooks {
            let home = hook.clone().restore(ctx)?.realm()?;
            hook.restore(&home)?.call::<_, ()>(())?;
        }
        Ok(())
    })();
    registry.borrow_mut().reactions.collecting = false;
    result
}

fn note_defined(ctx: &Ctx<'_>) -> Result<()> {
    let world = world(ctx)?;
    let frame = world.borrow().frame();
    world
        .borrow()
        .registry()
        .borrow_mut()
        .reactions
        .defined
        .insert(frame);
    Ok(())
}

fn push(ctx: &Ctx<'_>) -> Result<()> {
    collect(ctx)?;
    world(ctx)?
        .borrow()
        .registry()
        .borrow_mut()
        .reactions
        .stack
        .push(Vec::new());
    Ok(())
}

fn pop(ctx: &Ctx<'_>) -> Result<()> {
    let collection = collect(ctx);
    let registry = world(ctx)?.borrow().registry();
    let queue =
        registry.borrow_mut().reactions.stack.pop().ok_or_else(|| {
            Exception::throw_internal(ctx, "custom element reaction stack is empty")
        })?;
    let exception = collection
        .as_ref()
        .err()
        .filter(|error| error.is_exception())
        .map(|_| ctx.catch());
    invoke(ctx, &registry, queue);
    if let Some(exception) = exception {
        return Err(ctx.throw(exception));
    }
    collection
}

// https://html.spec.whatwg.org/multipage/custom-elements.html#invoke-custom-element-reactions
fn invoke(ctx: &Ctx<'_>, registry: &Rc<RefCell<RealmRegistry>>, queue: Vec<NodeId>) {
    for element in queue {
        loop {
            let reaction = registry
                .borrow_mut()
                .reactions
                .element_reactions
                .get_mut(&element)
                .and_then(VecDeque::pop_front);
            let Some(reaction) = reaction else { break };
            let result: Result<()> = (|| {
                let home = reaction.callback.clone().restore(ctx)?.realm()?;
                let callback = reaction.callback.restore(&home)?;
                let element = reaction.element.restore(&home)?;
                let arguments = reaction.arguments.restore(&home)?;
                let mut call = Args::new(home.clone(), arguments.len());
                call.this(element)?;
                for argument in arguments.iter::<Value>() {
                    call.push_arg(argument?)?;
                }
                if let Err(error) = callback.call_arg::<()>(call) {
                    host::report_callback_error(&home, &error);
                }
                Ok(())
            })();
            if let Err(error) = result {
                host::report_callback_error(ctx, &error);
            }
        }
        registry
            .borrow_mut()
            .reactions
            .element_reactions
            .remove(&element);
    }
}

#[derive(Clone, Copy)]
enum Bridge {
    Enqueue,
    Push,
    Pop,
    DeliverBackup,
    NoteDefined,
}

fn dispatch<'js>(operation: host::Operation, params: &Params<'_, 'js>) -> Result<Value<'js>> {
    let operation = match operation.index() {
        0 => Bridge::Enqueue,
        1 => Bridge::Push,
        2 => Bridge::Pop,
        3 => Bridge::DeliverBackup,
        4 => Bridge::NoteDefined,
        _ => {
            return Err(Exception::throw_internal(
                params.ctx(),
                "unknown custom element bridge operation",
            ));
        }
    };
    operation.call(params)
}

impl Bridge {
    fn call<'js>(self, params: &Params<'_, 'js>) -> Result<Value<'js>> {
        let ctx = params.ctx();
        match self {
            Self::Push => push(ctx)?,
            Self::Pop => pop(ctx)?,
            Self::NoteDefined => note_defined(ctx)?,
            Self::DeliverBackup => {
                let registry = world(ctx)?.borrow().registry();
                loop {
                    let queue = std::mem::take(&mut registry.borrow_mut().reactions.backup);
                    if queue.is_empty() {
                        break;
                    }
                    invoke(ctx, &registry, queue);
                }
                registry.borrow_mut().reactions.backup_scheduled = false;
            }
            Self::Enqueue => {
                host::require_arguments(params, 3)?;
                let element = params
                    .arg(0)
                    .ok_or_else(|| Exception::throw_type(ctx, "missing reaction element"))?;
                let id = host_node_id(ctx, &element)
                    .ok_or_else(|| Exception::throw_type(ctx, "reaction element is not a node"))?;
                let element = Object::from_js(ctx, element)?;
                let callback = Function::from_js(
                    ctx,
                    params
                        .arg(1)
                        .ok_or_else(|| Exception::throw_type(ctx, "missing reaction callback"))?,
                )?;
                let arguments = Array::from_js(
                    ctx,
                    params
                        .arg(2)
                        .ok_or_else(|| Exception::throw_type(ctx, "missing reaction arguments"))?,
                )?;
                let world = world(ctx)?;
                let (registry, owner, microtask) = {
                    let world = world.borrow();
                    (
                        world.registry(),
                        world.frame(),
                        world.pristine_queue_microtask.clone(),
                    )
                };
                let schedule = {
                    let mut registry = registry.borrow_mut();
                    let reactions = &mut registry.reactions;
                    reactions
                        .element_reactions
                        .entry(id)
                        .or_default()
                        .push_back(Reaction {
                            element: Persistent::save(ctx, element),
                            callback: Persistent::save(ctx, callback),
                            arguments: Persistent::save(ctx, arguments),
                            owner,
                        });
                    if let Some(queue) = reactions.stack.last_mut() {
                        queue.push(id);
                        false
                    } else {
                        reactions.backup.push(id);
                        let schedule = !reactions.backup_scheduled;
                        reactions.backup_scheduled = true;
                        schedule
                    }
                };
                if schedule {
                    let result = (|| {
                        let microtask = microtask
                            .ok_or_else(|| {
                                Exception::throw_internal(
                                    ctx,
                                    "custom element microtask scheduler is missing",
                                )
                            })?
                            .restore(ctx)?;
                        let deliver = Function::new_native(
                            ctx.clone(),
                            host::HostCall::new(dispatch, host::Operation::new(3)),
                        )?;
                        microtask.call::<_, ()>((deliver,))
                    })();
                    if result.is_err() {
                        registry.borrow_mut().reactions.backup_scheduled = false;
                    }
                    result?;
                }
            }
        }
        Ok(Value::new_undefined(ctx.clone()))
    }
}

pub(crate) fn install(ctx: &Ctx<'_>) -> Result<()> {
    for (name, operation) in [
        ("__tbEnqueueCustomReaction", 0),
        ("__tbPushCustomReactions", 1),
        ("__tbPopCustomReactions", 2),
        ("__tbNoteCustomDefined", 4),
    ] {
        super::bridge::object(ctx)?.set(
            name,
            Function::new_native(
                ctx.clone(),
                host::HostCall::new(dispatch, host::Operation::new(operation)),
            )?,
        )?;
    }
    Ok(())
}

pub(crate) fn capture(ctx: &Ctx<'_>) -> Result<()> {
    let hook: Function = super::bridge::object(ctx)?.get("__tbCollectCustomReactions")?;
    let world = world(ctx)?;
    let world = world.borrow();
    world
        .registry()
        .borrow_mut()
        .reactions
        .hooks
        .insert(world.frame(), Persistent::save(ctx, hook));
    for name in [
        "__tbCollectCustomReactions",
        "__tbEnqueueCustomReaction",
        "__tbPushCustomReactions",
        "__tbPopCustomReactions",
        "__tbNoteCustomDefined",
    ] {
        super::bridge::object(ctx)?.remove(name)?;
    }
    Ok(())
}
