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
        impl Stream<Item = mi_temp_monitor::Data>,
        bluetooth::DeviceAccessError<description::LoungeTempMonitor>,
    > {
        self.bluetooth
            .ensure_connected_and_healthy(Arc::clone(&self.lounge_temp_monitor))
            .await?;
        let (data, notify) = self
            .lounge_temp_monitor
            .read()
            .await
            .get_connected()?
            .data_notify();
        Ok(stream! {
            loop {
                if let Some(last_data) = *data.lock().await {
                    yield last_data;
                }
                notify.notified().await;
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
