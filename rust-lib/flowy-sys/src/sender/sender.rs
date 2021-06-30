use crate::{
    error::{InternalError, SystemError},
    module::ModuleRequest,
    request::{EventRequest, Payload},
    response::EventResponse,
    sender::SenderRequest,
    service::{BoxService, Service, ServiceFactory},
    system::ModuleMap,
};
use futures_core::{future::LocalBoxFuture, ready, task::Context};
use std::{fmt::Debug, future::Future};
use tokio::{
    macros::support::{Pin, Poll},
    sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
};

macro_rules! service_factor_impl {
    ($name:ident) => {
        #[allow(non_snake_case, missing_docs)]
        impl<T> ServiceFactory<SenderRequest<T>> for $name<T>
        where
            T: 'static + Debug,
        {
            type Response = EventResponse;
            type Error = SystemError;
            type Service = BoxService<SenderRequest<T>, Self::Response, Self::Error>;
            type Context = ();
            type Future = LocalBoxFuture<'static, Result<Self::Service, Self::Error>>;

            fn new_service(&self, _cfg: Self::Context) -> Self::Future {
                let module_map = self.module_map.clone();
                let service = Box::new(SenderService { module_map });
                Box::pin(async move { Ok(service as Self::Service) })
            }
        }
    };
}

struct SenderService {
    module_map: ModuleMap,
}

impl<T> Service<SenderRequest<T>> for SenderService
where
    T: 'static + Debug,
{
    type Response = EventResponse;
    type Error = SystemError;
    type Future = LocalBoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn call(&self, data: SenderRequest<T>) -> Self::Future {
        let module_map = self.module_map.clone();
        let SenderRequest {
            config,
            event,
            payload,
            callback,
        } = data;

        let mut request = ModuleRequest::new(event.clone());
        if let Some(payload) = payload {
            request = request.payload(payload);
        }

        let fut = async move {
            let result = {
                match module_map.get(&event) {
                    Some(module) => {
                        let fut = module.new_service(());
                        let service_fut = fut.await?.call(request);
                        service_fut.await
                    },
                    None => {
                        let msg = format!("Can not find the module to handle the request:{:?}", request);
                        Err(InternalError::new(msg).into())
                    },
                }
            };

            let response = result.unwrap_or_else(|e| e.into());
            if let Some(callback) = callback {
                callback(config, response.clone());
            }

            Ok(response)
        };
        Box::pin(fut)
    }
}

pub struct Sender<T>
where
    T: 'static + Debug,
{
    module_map: ModuleMap,
    data_tx: UnboundedSender<SenderRequest<T>>,
    data_rx: Option<UnboundedReceiver<SenderRequest<T>>>,
}

service_factor_impl!(Sender);

impl<T> Sender<T>
where
    T: 'static + Debug,
{
    pub fn new(module_map: ModuleMap) -> Self {
        let (data_tx, data_rx) = unbounded_channel::<SenderRequest<T>>();
        Self {
            module_map,
            data_tx,
            data_rx: Some(data_rx),
        }
    }

    pub fn async_send(&self, data: SenderRequest<T>) { let _ = self.data_tx.send(data); }

    pub fn sync_send(&self, data: SenderRequest<T>) -> EventResponse {
        let factory = self.new_service(());

        futures::executor::block_on(async {
            let service = factory.await.unwrap();
            service.call(data).await.unwrap()
        })
    }

    pub fn take_rx(&mut self) -> UnboundedReceiver<SenderRequest<T>> { self.data_rx.take().unwrap() }
}

pub struct SenderRunner<T>
where
    T: 'static + Debug,
{
    module_map: ModuleMap,
    data_rx: UnboundedReceiver<SenderRequest<T>>,
}

service_factor_impl!(SenderRunner);

impl<T> SenderRunner<T>
where
    T: 'static + Debug,
{
    pub fn new(module_map: ModuleMap, data_rx: UnboundedReceiver<SenderRequest<T>>) -> Self {
        Self { module_map, data_rx }
    }
}

impl<T> Future for SenderRunner<T>
where
    T: 'static + Debug,
{
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        loop {
            match ready!(Pin::new(&mut self.data_rx).poll_recv(cx)) {
                None => return Poll::Ready(()),
                Some(ctx) => {
                    let factory = self.new_service(());
                    tokio::task::spawn_local(async move {
                        let service = factory.await.unwrap();
                        let _ = service.call(ctx).await;
                    });
                },
            }
        }
    }
}
