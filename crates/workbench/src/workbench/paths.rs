//! System path selection through GPUI's platform dialog APIs.

use super::*;
use futures::FutureExt as _;
use retrowork_platform::PathRequest;

impl Workbench {
    /// Delivers confirmed paths to the requesting command; cancellation never invokes it.
    pub(super) fn choose_path(
        &mut self,
        request: PathRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.path_prompt.is_some() {
            return;
        }
        let (kind, command, response) = match request {
            PathRequest::Folder(request) => {
                let response = cx.prompt_for_paths(gpui::PathPromptOptions {
                    files: false,
                    directories: true,
                    multiple: false,
                    prompt: Some(request.confirm.into()),
                });
                (
                    "folder",
                    request.command,
                    async move {
                        let paths = response
                            .await
                            .map_err(|e| e.to_string())?
                            .map_err(|e| e.to_string())?;
                        Ok::<_, String>(
                            paths
                                .filter(|paths| paths.len() == 1)
                                .map(|paths| serde_json::json!({"directory": paths[0]})),
                        )
                    }
                    .boxed_local(),
                )
            }
            PathRequest::Save(request) => {
                let response = cx.prompt_for_new_path(&request.directory, Some(&request.name));
                (
                    "save-file",
                    request.command,
                    async move {
                        let path = response
                            .await
                            .map_err(|e| e.to_string())?
                            .map_err(|e| e.to_string())?;
                        Ok(path.map(
                            |path| serde_json::json!({"document": request.document, "path": path}),
                        ))
                    }
                    .boxed_local(),
                )
            }
        };
        self.platform
            .shell_mut()
            .trace_mut()
            .record("dialog.opened", kind);
        self.path_prompt = Some(cx.spawn_in(window, async move |this, cx| {
            let result = response.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.path_prompt = None;
                this.platform
                    .shell_mut()
                    .trace_mut()
                    .record("dialog.closed", kind);
                this.focus.focus(window, cx);
                match result {
                    Ok(Some(args)) => this.execute(&command, Some(&args), window, cx),
                    Ok(None) => cx.notify(),
                    Err(reason) => {
                        let message = format!("Cannot open system picker: {reason}");
                        this.platform.shell_mut().set_status(&message);
                        this.platform.shell().output().append(message, Tone::Error);
                        cx.notify();
                    }
                }
            });
        }));
    }
}
