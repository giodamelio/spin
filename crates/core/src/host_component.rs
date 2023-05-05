use std::{any::Any, marker::PhantomData, sync::Arc};

use anyhow::Result;

use super::{Data, Linker};

/// A trait for Spin "host components".
///
/// A Spin host component is an interface provided to Spin components that is
/// implemented by the host. This trait is designed to be compatible with
/// [`wit-bindgen`](https://github.com/bytecodealliance/wasmtime/tree/main/crates/wit-bindgen)'s
/// generated bindings.
///
/// # Example
///
/// ```ignore
/// use spin_core::my_interface;
///
/// #[derive(Default)]
/// struct MyHostComponent {
///     // ...
/// }
///
/// #[async_trait]
/// impl my_interface::Host for MyHostComponent {
///     // ...
/// }
///
/// impl HostComponent for MyHostComponent {
///     type Data = Self;
///
///     fn add_to_linker<T: Send>(
///         linker: &mut Linker<T>,
///         get: impl Fn(&mut spin_core::Data<T>) -> &mut Self::Data + Send + Sync + Copy + 'static,
///     ) -> anyhow::Result<()> {
///         my_interface::add_to_linker(linker, get)
///     }
///
///     fn build_data(&self) -> Self::Data {
///         Default::default()
///     }
/// }
/// ```
pub trait HostComponent: Send + Sync + 'static {
    /// Host component runtime data.
    type Data: Send + Sized + 'static;

    /// Add this component to the given Linker, using the given runtime state-getting handle.
    ///
    /// This function signature mirrors those generated by `wit-bindgen`.
    fn add_to_linker<T: Send>(
        linker: &mut Linker<T>,
        get: impl Fn(&mut Data<T>) -> &mut Self::Data + Send + Sync + Copy + 'static,
    ) -> Result<()>;

    /// Builds new host component runtime data for [`HostComponentsData`].
    fn build_data(&self) -> Self::Data;
}

impl<HC: HostComponent> HostComponent for Arc<HC> {
    type Data = HC::Data;

    fn add_to_linker<T: Send>(
        linker: &mut Linker<T>,
        get: impl Fn(&mut Data<T>) -> &mut Self::Data + Send + Sync + Copy + 'static,
    ) -> Result<()> {
        HC::add_to_linker(linker, get)
    }

    fn build_data(&self) -> Self::Data {
        (**self).build_data()
    }
}

/// An opaque handle returned by [`crate::EngineBuilder::add_host_component`]
/// which can be passed to [`HostComponentsData`] to access or set associated
/// [`HostComponent::Data`].
pub struct HostComponentDataHandle<HC: HostComponent> {
    idx: usize,
    _phantom: PhantomData<fn() -> HC::Data>,
}

impl<HC: HostComponent> Copy for HostComponentDataHandle<HC> {}

impl<HC: HostComponent> Clone for HostComponentDataHandle<HC> {
    fn clone(&self) -> Self {
        Self {
            idx: self.idx,
            _phantom: PhantomData,
        }
    }
}

/// An opaque handle which can be passed to [`HostComponentsData`] to access or
/// set associated [`HostComponent::Data`].
#[derive(Clone, Copy)]
pub struct AnyHostComponentDataHandle(usize);

impl<T: HostComponent> From<HostComponentDataHandle<T>> for AnyHostComponentDataHandle {
    fn from(value: HostComponentDataHandle<T>) -> Self {
        Self(value.idx)
    }
}

#[doc(hidden)]
pub trait DynSafeHostComponent {
    fn build_data_box(&self) -> Box<dyn Any + Send>;
}

impl<T: HostComponent> DynSafeHostComponent for T
where
    T::Data: Any + Send,
{
    fn build_data_box(&self) -> Box<dyn Any + Send> {
        Box::new(self.build_data())
    }
}

type BoxHostComponent = Box<dyn DynSafeHostComponent + Send + Sync>;

#[derive(Default)]
pub struct HostComponentsBuilder {
    host_components: Vec<BoxHostComponent>,
}

impl HostComponentsBuilder {
    pub fn add_host_component<T: Send, HC: HostComponent>(
        &mut self,
        linker: &mut Linker<T>,
        host_component: HC,
    ) -> Result<HostComponentDataHandle<HC>> {
        let idx = self.host_components.len();
        self.host_components.push(Box::new(host_component));
        HC::add_to_linker(linker, move |data| {
            data.host_components_data
                .get_or_insert_idx(idx)
                .downcast_mut()
                .unwrap()
        })?;
        Ok(HostComponentDataHandle::<HC> {
            idx,
            _phantom: PhantomData,
        })
    }

    pub fn build(self) -> HostComponents {
        HostComponents {
            host_components: Arc::new(self.host_components),
        }
    }
}

pub struct HostComponents {
    host_components: Arc<Vec<BoxHostComponent>>,
}

impl HostComponents {
    pub fn builder() -> HostComponentsBuilder {
        Default::default()
    }

    pub fn new_data(&self) -> HostComponentsData {
        // Fill with `None`
        let data = std::iter::repeat_with(Default::default)
            .take(self.host_components.len())
            .collect();
        HostComponentsData {
            data,
            host_components: self.host_components.clone(),
        }
    }
}

/// Holds a heterogenous set of [`HostComponent::Data`]s.
pub struct HostComponentsData {
    data: Vec<Option<Box<dyn Any + Send>>>,
    host_components: Arc<Vec<BoxHostComponent>>,
}

impl HostComponentsData {
    /// Sets the [`HostComponent::Data`] for the given `handle`.
    pub fn set<HC: HostComponent>(&mut self, handle: HostComponentDataHandle<HC>, data: HC::Data) {
        self.data[handle.idx] = Some(Box::new(data));
    }

    /// Retrieves a mutable reference to [`HostComponent::Data`] for the given `handle`.
    ///
    /// If unset, the data will be initialized with [`HostComponent::build_data`].
    pub fn get_or_insert<HC: HostComponent>(
        &mut self,
        handle: HostComponentDataHandle<HC>,
    ) -> &mut HC::Data {
        let x = self.get_or_insert_idx(handle.idx);
        x.downcast_mut().unwrap()
    }

    /// Retrieves a mutable reference to [`HostComponent::Data`] for the given `handle`.
    ///
    /// If unset, the data will be initialized with [`HostComponent::build_data`].
    pub fn get_or_insert_any(&mut self, handle: AnyHostComponentDataHandle) -> &mut dyn Any {
        self.get_or_insert_idx(handle.0)
    }

    fn get_or_insert_idx(&mut self, idx: usize) -> &mut Box<dyn Any + Send> {
        self.data[idx].get_or_insert_with(|| self.host_components[idx].build_data_box())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestHC;

    impl HostComponent for TestHC {
        type Data = u8;

        fn add_to_linker<T: Send>(
            _linker: &mut Linker<T>,
            _get: impl Fn(&mut Data<T>) -> &mut Self::Data + Send + Sync + Copy + 'static,
        ) -> Result<()> {
            Ok(())
        }

        fn build_data(&self) -> Self::Data {
            0
        }
    }

    #[test]
    fn host_components_data() {
        let engine = wasmtime::Engine::default();
        let mut linker: crate::Linker<()> = crate::Linker::new(&engine);

        let mut builder = HostComponents::builder();
        let handle1 = builder
            .add_host_component(&mut linker, Arc::new(TestHC))
            .unwrap();
        let handle2 = builder.add_host_component(&mut linker, TestHC).unwrap();
        let host_components = builder.build();
        let mut hc_data = host_components.new_data();

        assert_eq!(hc_data.get_or_insert(handle1), &0);

        hc_data.set(handle2, 1);
        assert_eq!(hc_data.get_or_insert(handle2), &1);
    }
}
