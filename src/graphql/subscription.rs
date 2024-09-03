use std::{ops::Deref, sync::Arc};

use async_graphql::Subscription;
use async_stream::stream;
use futures::Stream;

use crate::{
    bluetooth,
    device::{description, mi_temp_monitor},
    App,
};

pub struct SubscriptionRoot(pub(super) App);

#[Subscription]
impl SubscriptionRoot {
    async fn lounge_temp_monitor_data(
        &self,
    ) -> Result<
        impl Stream<Item = Option<mi_temp_monitor::Data>>,
        bluetooth::DeviceAccessError<description::LoungeTempMonitor>,
    > {
        self.bluetooth
            .ensure_connected_and_healthy(Arc::clone(&self.lounge_temp_monitor))
            .await?;
        let (shared_data, notify) = self
            .lounge_temp_monitor
            .read()
            .await
            .get_connected()?
            .data_notify();

        let mut last_data = *shared_data.lock().await;
        Ok(stream! {
            loop {
                yield last_data;
                notify.notified().await;

                last_data = *shared_data.lock().await;
                // It means that device is no longer available.
                // Do NOT perform this check before waiting for a notification,
                // because device may be just initialized and not received data yet.
                if last_data.is_none() {
                    break;
                }
            }
        })
    }
}

impl Deref for SubscriptionRoot {
    type Target = App;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
