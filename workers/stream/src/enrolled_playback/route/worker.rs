use std::{future::poll_fn, pin::pin};

use ::worker::{Context, Date, Env, Request, Response, Result};
use futures_channel::oneshot;
use futures_util::future::{select, Either};
use nntune_core::original_source::reader::AsyncReadControl;

use super::*;
use crate::{
    original_play::worker::{channel, WorkerOriginalControl},
    play_route::PLAY_CONTENT_TYPE,
    runtime::{api_error_response, report_delivered},
};

mod source;

pub(crate) async fn serve(
    req: Request,
    env: Env,
    ctx: Context,
    request: PlayRouteRequest,
    secret: String,
) -> Result<Response> {
    let now = || UNIX_EPOCH + Duration::from_millis(Date::now().as_millis());
    let Ok((grant, _)) = grant::verify_private(
        request.token.as_deref().unwrap_or(""),
        secret.as_bytes(),
        Date::now().as_millis() / 1000,
    ) else {
        return api_error_response(&ApiError::unauthorized(), None);
    };
    let mut control = WorkerOriginalControl::until(grant.exp_unix, &req)?;
    let (ready, response) = oneshot::channel();
    ctx.wait_until(async move {
        let source = source::WorkerSource::from_env(&env, request.token.as_deref().unwrap_or(""));
        let mut ready = Some(ready);
        let mut delivered = None;
        let result = {
            let play = super::serve(
                &request,
                secret.as_bytes(),
                source.as_ref(),
                now,
                |range| {
                    let (response, writer) = if range.status == 416 {
                        let value = range.content_range.as_deref();
                        let extra = value.map(|value| ("content-range", value));
                        api_error_response(&ApiError::range_not_satisfiable(), extra)
                            .map(|response| (response, None))
                    } else {
                        let mut headers = vec![
                            ("content-type".into(), PLAY_CONTENT_TYPE.into()),
                            ("cache-control".into(), "private, no-store".into()),
                            ("accept-ranges".into(), "bytes".into()),
                        ];
                        if let Some(value) = range.content_range {
                            headers.push(("content-range".into(), value));
                        }
                        channel(range.status, &headers, range.len)
                            .map(|(response, writer)| (response, Some(writer)))
                    }
                    .map_err(|_| ApiError::internal_error())?;
                    ready
                        .take()
                        .expect("response starts once")
                        .send(Ok(response))
                        .map_err(|_| ApiError::upstream_unavailable())?;
                    Ok(writer)
                },
                |interval| delivered = Some(interval),
            );
            match select(pin!(poll_fn(|cx| control.poll_cancel(cx))), pin!(play)).await {
                Either::Left(_) => Err(ApiError::unauthorized()),
                Either::Right((result, _)) => result,
            }
        };
        if let Some(ready) = ready {
            let error = result.err().unwrap_or_else(ApiError::internal_error);
            let _ = ready.send(api_error_response(&error, None));
        }
        if let Some(interval) = delivered {
            report_delivered(env, interval).await;
        }
    });
    response
        .await
        .unwrap_or_else(|_| api_error_response(&ApiError::internal_error(), None))
}
