use super::{CollectionKind, child_value, live_collection, world, wrap_node};

use crate::dom::tree::NodeId;

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use rquickjs::{
    Ctx, Exception, Function, Object, Persistent, Result, Value, class::Trace, prelude::This,
};

use crate::js::world::{Handle, ObserverOptions, ReadyObserver, RealmRegistry, RecordData};

// https://dom.spec.whatwg.org/#interface-mutationrecord
#[derive(Trace, rquickjs::JsLifetime)]
pub struct JsMutationRecord<'js> {
    record: RecordData,
    target: Value<'js>,
    added_nodes: Value<'js>,
    removed_nodes: Value<'js>,
}

impl<'js> JsMutationRecord<'js> {
    // https://dom.spec.whatwg.org/#interface-mutationrecord
    fn new(ctx: &Ctx<'js>, mut record: RecordData) -> Result<Self> {
        let target = wrap_node(ctx, record.target.0)?;
        let added_nodes = node_list(ctx, record.target.0, std::mem::take(&mut record.added))?;
        let removed_nodes = node_list(ctx, record.target.0, std::mem::take(&mut record.removed))?;
        Ok(Self {
            record,
            target,
            added_nodes,
            removed_nodes,
        })
    }
}

impl<'js> mutation_record_generated::MutationRecord<'js> for JsMutationRecord<'js> {
    fn get_type(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        rquickjs::String::from_str(ctx.clone(), &self.record.typ)
    }

    fn get_target(&self, _ctx: &Ctx<'js>) -> Result<Value<'js>> {
        Ok(self.target.clone())
    }

    fn get_added_nodes(&self, _ctx: &Ctx<'js>) -> Result<Value<'js>> {
        Ok(self.added_nodes.clone())
    }

    fn get_removed_nodes(&self, _ctx: &Ctx<'js>) -> Result<Value<'js>> {
        Ok(self.removed_nodes.clone())
    }

    fn get_previous_sibling(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        child_value(ctx, self.record.previous.map(|handle| handle.0))
    }

    fn get_next_sibling(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        child_value(ctx, self.record.next.map(|handle| handle.0))
    }

    fn get_attribute_name(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        self.record
            .attribute_name
            .as_deref()
            .map(|name| rquickjs::String::from_str(ctx.clone(), name))
            .transpose()
    }

    fn get_attribute_namespace(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        self.record
            .attribute_namespace
            .as_deref()
            .map(|namespace| rquickjs::String::from_str(ctx.clone(), namespace))
            .transpose()
    }

    fn get_old_value(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        self.record
            .old_value
            .as_ref()
            .map(|value| super::dom_string(ctx, value))
            .transpose()
    }
}

include!(concat!(env!("OUT_DIR"), "/MutationRecord.rs"));
include!(concat!(env!("OUT_DIR"), "/MutationObserver.rs"));

fn node_list<'js>(ctx: &Ctx<'js>, scope: NodeId, nodes: Vec<Handle>) -> Result<Value<'js>> {
    live_collection(ctx, scope, CollectionKind::Static(nodes), None)
}

// https://dom.spec.whatwg.org/#interface-mutationobserver
#[derive(Trace, rquickjs::JsLifetime)]
pub struct JsMutationObserver {
    pub(crate) id: u64,
    #[qjs(skip_trace)]
    registry: Weak<RefCell<RealmRegistry>>,
}

impl JsMutationObserver {
    fn registry(&self, ctx: &Ctx<'_>) -> Result<Rc<RefCell<RealmRegistry>>> {
        self.registry
            .upgrade()
            .ok_or_else(|| Exception::throw_type(ctx, "observer realm is gone"))
    }
}

impl<'js> mutation_observer_generated::MutationObserver<'js> for JsMutationObserver {
    // https://dom.spec.whatwg.org/#dom-mutationobserver-mutationobserver
    fn constructor(ctx: &Ctx<'js>, callback: Function<'js>) -> Result<Self> {
        let world_rc = world(ctx)?;
        let (registry, owner) = {
            let world = world_rc.borrow();
            (Rc::clone(&world.runtime.registry), world.frame())
        };
        let id = registry
            .borrow_mut()
            .observers
            .create(owner, Persistent::save(ctx, callback));
        Ok(Self {
            id,
            registry: Rc::downgrade(&registry),
        })
    }

    // https://dom.spec.whatwg.org/#dom-mutationobserver-observe
    fn observe(
        &self,
        ctx: Ctx<'js>,
        observer_object: Object<'js>,
        target: super::host::NodeReference,
        options: mutation_observer_generated::MutationObserverInit,
    ) -> Result<()> {
        // https://webidl.spec.whatwg.org/#es-dictionary
        let attributes_present = options.attributes.is_some();
        let attributes = options.attributes.unwrap_or(false);
        let attribute_old_value_present = options.attribute_old_value.is_some();
        let attribute_old_value = options.attribute_old_value.unwrap_or(false);
        let character_data_present = options.character_data.is_some();
        let character_data = options.character_data.unwrap_or(false);
        let character_data_old_value_present = options.character_data_old_value.is_some();
        let character_data_old_value = options.character_data_old_value.unwrap_or(false);
        let attribute_filter = options.attribute_filter;
        let parsed = ObserverOptions {
            child_list: options.child_list,
            attributes: attributes
                || (!attributes_present
                    && (attribute_old_value_present || attribute_filter.is_some())),
            character_data: character_data
                || (!character_data_present && character_data_old_value_present),
            subtree: options.subtree,
            attribute_old_value,
            character_data_old_value,
            attribute_filter,
        };
        if !(parsed.child_list || parsed.attributes || parsed.character_data) {
            return Err(Exception::throw_type(
                &ctx,
                "options must set childList, attributes, or characterData",
            ));
        }
        if !parsed.attributes && parsed.attribute_old_value {
            return Err(Exception::throw_type(
                &ctx,
                "attributeOldValue requires attributes",
            ));
        }
        if !parsed.attributes && parsed.attribute_filter.is_some() {
            return Err(Exception::throw_type(
                &ctx,
                "attributeFilter requires attributes",
            ));
        }
        if !parsed.character_data && parsed.character_data_old_value {
            return Err(Exception::throw_type(
                &ctx,
                "characterDataOldValue requires characterData",
            ));
        }
        let runtime = world(&ctx)?.borrow().runtime.clone();
        self.registry(&ctx)?.borrow_mut().observers.observe(
            self.id,
            target,
            parsed,
            Persistent::save(&ctx, observer_object),
            &mut runtime.documents.borrow_mut(),
        );
        schedule_mutation_delivery(&ctx)
    }

    // https://dom.spec.whatwg.org/#dom-mutationobserver-disconnect
    fn disconnect(&self, ctx: Ctx<'js>) -> Result<()> {
        let runtime = world(&ctx)?.borrow().runtime.clone();
        self.registry(&ctx)?
            .borrow_mut()
            .observers
            .disconnect(self.id, &mut runtime.documents.borrow_mut());
        schedule_mutation_delivery(&ctx)
    }

    // https://dom.spec.whatwg.org/#dom-mutationobserver-takerecords
    fn take_records(&self, ctx: Ctx<'js>) -> Result<Vec<Value<'js>>> {
        let runtime = world(&ctx)?.borrow().runtime.clone();
        let queue = self
            .registry(&ctx)?
            .borrow_mut()
            .observers
            .take_records(self.id, &mut runtime.documents.borrow_mut());
        schedule_mutation_delivery(&ctx)?;
        queue
            .into_iter()
            .map(|record| {
                let record = JsMutationRecord::new(&ctx, record)?;
                Ok(super::host::instance(&ctx, record)?.into_value())
            })
            .collect()
    }
}

// https://dom.spec.whatwg.org/#notify-mutation-observers
pub(crate) fn deliver_mutations(ctx: Ctx<'_>) -> Result<()> {
    let runtime = world(&ctx)?.borrow().runtime.clone();
    let pending = runtime
        .registry
        .borrow_mut()
        .observers
        .begin_notification(&mut runtime.documents.borrow_mut());
    for id in pending {
        let ready = runtime.registry.borrow_mut().observers.delivery(id);
        if let Some(observer) = ready {
            deliver_observer(&ctx, observer);
        }
    }
    Ok(())
}

// https://dom.spec.whatwg.org/#notify-mutation-observers
fn deliver_observer(delivery: &Ctx<'_>, observer: ReadyObserver) {
    let step: Result<()> = (|| {
        let home = observer.callback.clone().restore(delivery)?.realm()?;
        let callback = observer.callback.restore(&home)?;
        let records = observer
            .records
            .into_iter()
            .map(|record| {
                super::host::instance(&home, JsMutationRecord::new(&home, record)?)
                    .map(rquickjs::Class::into_value)
            })
            .collect::<Result<Vec<_>>>()?;
        let array = super::host::sequence(&home, records)?;
        let object = observer.object.restore(&home)?;
        if let Err(error) = callback.call::<_, ()>((This(object.clone()), array, object)) {
            super::host::report_callback_error(&home, &error);
        }
        Ok(())
    })();
    if let Err(error) = step {
        super::host::report_callback_error(delivery, &error);
    }
}

// https://dom.spec.whatwg.org/#queue-a-mutation-record
pub(crate) fn drain_mutation_journal(ctx: &Ctx<'_>) -> Result<()> {
    let runtime = world(ctx)?.borrow().runtime.clone();
    runtime
        .registry
        .borrow_mut()
        .observers
        .drain(&mut runtime.documents.borrow_mut());
    Ok(())
}

pub(crate) fn schedule_mutation_delivery(ctx: &Ctx<'_>) -> Result<()> {
    // https://dom.spec.whatwg.org/#queue-a-mutation-observer-microtask
    let (runtime, pristine) = {
        let world = world(ctx)?;
        let world = world.borrow();
        (
            world.runtime.clone(),
            world
                .deliver_mutations_fn
                .clone()
                .zip(world.pristine_queue_microtask.clone()),
        )
    };
    {
        let mut registry = runtime.registry.borrow_mut();
        let (_, observed) = registry
            .observers
            .drain(&mut runtime.documents.borrow_mut());
        if !registry.observers.needs_notification() && !observed {
            return Ok(());
        }
        if registry.observers.scheduled {
            return Ok(());
        }
        registry.observers.scheduled = true;
    }
    let result = (|| {
        let (deliver, queue) = if let Some((deliver, queue)) = pristine {
            (deliver.restore(ctx)?, queue.restore(ctx)?)
        } else {
            (
                crate::js::bridge::object(ctx)?.get::<_, Function>("__tb_deliver_mutations")?,
                ctx.globals().get::<_, Function>("queueMicrotask")?,
            )
        };
        queue.call::<_, ()>((deliver,))
    })();
    if result.is_err() {
        runtime.registry.borrow_mut().observers.scheduled = false;
    }
    result
}
