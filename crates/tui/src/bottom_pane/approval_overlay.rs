use crossterm::event::KeyCode;
use devo_protocol::ApprovalDecisionValue;
use devo_protocol::ApprovalScopeValue;
use devo_protocol::SessionId;
use devo_protocol::TurnId;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Stylize;
use ratatui::text::Line;

use crate::app_command::AppCommand;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::CancellationEvent;
use crate::bottom_pane::bottom_pane_view::BottomPaneView;
use crate::bottom_pane::list_selection_view::ListSelectionView;
use crate::bottom_pane::list_selection_view::SelectionItem;
use crate::bottom_pane::list_selection_view::SelectionViewParams;
use crate::key_hint;
use crate::render::renderable::ColumnRenderable;
use crate::render::renderable::Renderable;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApprovalOverlayRequest {
    pub(crate) session_id: SessionId,
    pub(crate) turn_id: TurnId,
    pub(crate) approval_id: String,
    pub(crate) action_summary: String,
    pub(crate) justification: String,
    pub(crate) resource: Option<String>,
    pub(crate) available_scopes: Vec<String>,
    pub(crate) path: Option<String>,
    pub(crate) host: Option<String>,
    pub(crate) target: Option<String>,
}

pub(crate) struct ApprovalOverlay {
    list: ListSelectionView,
}

impl ApprovalOverlay {
    pub(crate) fn new(
        request: ApprovalOverlayRequest,
        app_event_tx: AppEventSender,
        accent_color: Color,
    ) -> Self {
        Self {
            list: ListSelectionView::new(build_params(request), app_event_tx, accent_color),
        }
    }
}

impl BottomPaneView for ApprovalOverlay {
    fn handle_key_event(&mut self, key_event: crossterm::event::KeyEvent) {
        self.list.handle_key_event(key_event);
    }

    fn is_complete(&self) -> bool {
        self.list.is_complete()
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.list.on_ctrl_c()
    }
}

impl Renderable for ApprovalOverlay {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.list.render(area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.list.desired_height(width)
    }
}

fn build_params(request: ApprovalOverlayRequest) -> SelectionViewParams {
    let header = build_header(&request);
    let mut items = Vec::new();
    if request.available_scopes.is_empty()
        || request
            .available_scopes
            .iter()
            .any(|scope| scope.eq_ignore_ascii_case("once"))
    {
        items.push(approval_item(
            "Approve once",
            "Allow only this pending tool execution.",
            KeyCode::Char('y'),
            &request,
            ApprovalDecisionValue::Approve,
            ApprovalScopeValue::Once,
        ));
    }
    if request
        .available_scopes
        .iter()
        .any(|scope| scope.eq_ignore_ascii_case("session"))
    {
        items.push(approval_item(
            "Approve for session",
            "Allow matching requests for the rest of this session.",
            KeyCode::Char('s'),
            &request,
            ApprovalDecisionValue::Approve,
            ApprovalScopeValue::Session,
        ));
    }
    items.push(approval_item(
        "Deny",
        "Reject this tool execution.",
        KeyCode::Char('n'),
        &request,
        ApprovalDecisionValue::Deny,
        ApprovalScopeValue::Once,
    ));

    SelectionViewParams {
        title: Some("Permission approval required".to_string()),
        footer_hint: Some(Line::from(
            "Use ↑/↓ to choose, Enter to confirm, Esc to cancel.",
        )),
        header: Box::new(header),
        items,
        on_cancel: Some(Box::new(move |app_event_tx| {
            app_event_tx.send(AppEvent::Command(AppCommand::ApprovalRespond {
                session_id: request.session_id,
                turn_id: request.turn_id,
                approval_id: request.approval_id.clone(),
                decision: ApprovalDecisionValue::Cancel,
                scope: ApprovalScopeValue::Once,
            }));
        })),
        ..Default::default()
    }
}

fn approval_item(
    name: &str,
    description: &str,
    shortcut: KeyCode,
    request: &ApprovalOverlayRequest,
    decision: ApprovalDecisionValue,
    scope: ApprovalScopeValue,
) -> SelectionItem {
    let session_id = request.session_id;
    let turn_id = request.turn_id;
    let approval_id = request.approval_id.clone();
    SelectionItem {
        name: name.to_string(),
        display_shortcut: Some(key_hint::plain(shortcut)),
        description: Some(description.to_string()),
        dismiss_on_select: true,
        actions: vec![Box::new(move |app_event_tx| {
            app_event_tx.send(AppEvent::Command(AppCommand::ApprovalRespond {
                session_id,
                turn_id,
                approval_id: approval_id.clone(),
                decision: decision.clone(),
                scope: scope.clone(),
            }));
        })],
        ..Default::default()
    }
}

fn build_header(request: &ApprovalOverlayRequest) -> ColumnRenderable<'static> {
    let mut header = ColumnRenderable::new();
    header.push(Line::from(request.action_summary.clone()).bold());
    header.push(Line::from(""));
    push_field(&mut header, "reason", Some(&request.justification));
    push_field(&mut header, "resource", request.resource.as_ref());
    push_field(&mut header, "path", request.path.as_ref());
    push_field(&mut header, "host", request.host.as_ref());
    push_field(&mut header, "target", request.target.as_ref());
    header
}

fn push_field(header: &mut ColumnRenderable<'static>, label: &str, value: Option<&String>) {
    let Some(value) = value else {
        return;
    };
    if value.trim().is_empty() {
        return;
    }
    header.push(Line::from(format!("{label}: {value}")).dim());
}
