use std::collections::HashMap;use std::path::PathBuf;use std::sync::Arc;
use neenee_agent::{AgentIdentity,PrincipalProfile};use neenee_core::{AgentRequest,AgentResponse,SessionOverview};
use neenee_persistence::session::SessionStore;use tokio::sync::{Mutex,broadcast,mpsc};use tokio_util::sync::CancellationToken;
use crate::UiBridge;use crate::bootstrap::{self,BootstrapParams};use crate::serve::AttachAction;
#[derive(Clone)]pub struct HostParams{pub identity:AgentIdentity,pub principal:PrincipalProfile,pub ui:Arc<dyn UiBridge>,pub project_root:PathBuf,}
pub struct HostedSession{pub session:Arc<SessionStore>,pub req_tx:mpsc::UnboundedSender<AgentRequest>,pub events:broadcast::Sender<AgentResponse>,pub cancel:CancellationToken,}
#[derive(Clone)]pub struct BoundSession{pub session:Arc<SessionStore>,pub req_tx:mpsc::UnboundedSender<AgentRequest>,pub events:broadcast::Sender<AgentResponse>,}
pub enum ResolveOutcome{Welcome(BoundSession),Pick{sessions:Vec<SessionOverview>},Error(String),}
#[derive(Clone)]pub struct SessionRegistry{params:Option<HostParams>,sessions:Arc<Mutex<HashMap<String,Arc<HostedSession>>>>,}
impl SessionRegistry{
pub fn new(params:HostParams)->Self{Self{params:Some(params),sessions:Arc::new(Mutex::new(HashMap::new())),}}
pub fn prehost_only()->Self{Self{params:None,sessions:Arc::new(Mutex::new(HashMap::new())),}}
pub async fn resolve(&self,action:AttachAction)->ResolveOutcome{match action{
AttachAction::New=>match self.assemble_hosted(crate::startup::StartupMode::Fresh).await{Ok(b)=>ResolveOutcome::Welcome(b),Err(AssembleErr::NoHost)=>ResolveOutcome::Error("this host cannot create sessions".into()),Err(AssembleErr::AssembleFailed(e))=>ResolveOutcome::Error(format!("could not start a new session: {e}")),},
AttachAction::Attach(None)=>self.resolve_auto().await,
AttachAction::Attach(Some(id))=>self.resolve_id(&id).await,}}
pub async fn host(&self,entry:HostedSession)->BoundSession{let id=entry.session.id().await;let b=BoundSession{session:entry.session.clone(),req_tx:entry.req_tx.clone(),events:entry.events.clone()};self.sessions.lock().await.insert(id,Arc::new(entry));b}
async fn resolve_auto(&self)->ResolveOutcome{let map=self.sessions.lock().await;match map.len(){0=>{drop(map);match self.assemble_hosted(crate::startup::StartupMode::Fresh).await{Ok(b)=>ResolveOutcome::Welcome(b),Err(AssembleErr::NoHost)=>ResolveOutcome::Error("no session is available on this host".into()),Err(AssembleErr::AssembleFailed(e))=>ResolveOutcome::Error(format!("could not start a session: {e}")),}},1=>{if let Some(e)=map.values().next(){ResolveOutcome::Welcome(self.bound_from(e))}else{unreachable!("len==1")}},_=>ResolveOutcome::Pick{sessions:self.overview(&map).await},}}
async fn resolve_id(&self,id:&str)->ResolveOutcome{{let map=self.sessions.lock().await;if let Some(e)=map.get(id){return ResolveOutcome::Welcome(self.bound_from(e));}}let Some(p)=&self.params else{return ResolveOutcome::Error(format!("session '{id}' is not hosted on this server"));};if !session_exists_on_disk(&p.project_root,id).await{return ResolveOutcome::Error(format!("unknown session id '{id}'"));}match self.assemble_hosted(crate::startup::StartupMode::Resume(Some(id.to_string()))).await{Ok(b)=>ResolveOutcome::Welcome(b),Err(AssembleErr::NoHost)=>ResolveOutcome::Error(format!("session '{id}' is not hosted on this server")),Err(AssembleErr::AssembleFailed(e))=>ResolveOutcome::Error(format!("could not resume session {id}: {e}")),}}
async fn assemble_hosted(&self,startup:crate::startup::StartupMode)->Result<BoundSession,AssembleErr>{let HostParams{identity,principal,ui,project_root}=self.params.as_ref().ok_or(AssembleErr::NoHost)?.clone();let boot=bootstrap::assemble(BootstrapParams{identity,principal,ui,startup,project_root:Some(project_root),autopilot:false,single_instance:false,}).await.map_err(AssembleErr::AssembleFailed)?;
let session=boot.session.clone();let req_tx=boot.req_tx.clone();
let(events_tx,_)=broadcast::channel::<AgentResponse>(1024);let tap=events_tx.clone();let mut rr=boot.resp_rx;tokio::spawn(async move{while let Some(r)=rr.recv().await{let _=tap.send(r);}});
let cancel=CancellationToken::new();let cd=cancel.clone();let driver=boot.driver;tokio::spawn(async move{tokio::select!{_=cd.cancelled()=>tracing::info!("registry: driver cancelled"),_=driver.run()=>tracing::info!("registry: driver exited"),}});
let id=session.id().await;let bound=BoundSession{session:session.clone(),req_tx:req_tx.clone(),events:events_tx.clone()};self.sessions.lock().await.insert(id,Arc::new(HostedSession{session,req_tx,events:events_tx,cancel}));Ok(bound)}
fn bound_from(&self,e:&Arc<HostedSession>)->BoundSession{BoundSession{session:e.session.clone(),req_tx:e.req_tx.clone(),events:e.events.clone(),}}
async fn overview(&self,map:&HashMap<String,Arc<HostedSession>>)->Vec<SessionOverview>{let mut out=Vec::new();for e in map.values(){out.push(overview_of(&e.session,true).await);}out.sort_by_key(|i|std::cmp::Reverse(i.updated_at));out}}
enum AssembleErr{NoHost,AssembleFailed(Box<dyn std::error::Error>),}
#[allow(clippy::collapsible_if)] async fn overview_of(session:&SessionStore,active:bool)->SessionOverview{let id=session.id().await;if let Ok(items)=session.list().await{if let Some(item)=items.into_iter().find(|i|i.id==id){return SessionOverview{id:item.id,overview:item.overview,created_at:item.created_at,updated_at:item.updated_at,message_count:item.message_count,active,};}}
let mc=session.full_transcript().await.len();SessionOverview{id,overview:String::new(),created_at:0,updated_at:0,message_count:mc,active}}
async fn session_exists_on_disk(project_root:&std::path::Path,id:&str)->bool{SessionStore::load_for_project(project_root.to_path_buf()).list().await.map(|items|items.iter().any(|i|i.id==id)).unwrap_or(false)}
