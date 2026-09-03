//! Global agent inventory through the explicit daemon plane.

use lait::control::{
    AgentInventoryAudience, AgentInventoryMutationSetting, AgentLifecycleSetting,
    AgentListItemView, AgentStateRevision, AgentView, ControlRoute, Request, Response,
};

use super::{Client, ClientError, ClientResult};

impl Client {
    pub async fn agent_create(
        &self,
        name: String,
        introduction: String,
    ) -> ClientResult<AgentView> {
        self.agent_view_request(Request::AgentCreate { name, introduction })
            .await
    }

    pub async fn agent_list(&self) -> ClientResult<Vec<AgentListItemView>> {
        match self.agent_request(Request::AgentList).await? {
            Response::Agents { agents } => Ok(agents),
            other => Err(ClientError::internal(format!(
                "agent list returned {other:?}"
            ))),
        }
    }

    pub async fn agent_show(&self, agent: String) -> ClientResult<AgentView> {
        match self
            .agent_request(Request::AgentShow {
                agent,
                audience: AgentInventoryAudience::Owner,
            })
            .await?
        {
            Response::Agent(agent) => Ok(*agent),
            other => Err(ClientError::internal(format!(
                "agent inventory returned {other:?}"
            ))),
        }
    }

    pub async fn agent_set_lifecycle(
        &self,
        agent: String,
        expected: AgentStateRevision,
        lifecycle: AgentLifecycleSetting,
    ) -> ClientResult<AgentView> {
        self.agent_view_request(Request::AgentSetLifecycle {
            agent,
            expected,
            lifecycle,
        })
        .await
    }

    pub async fn agent_inventory_mutate(
        &self,
        agent: String,
        expected: AgentStateRevision,
        mutation: AgentInventoryMutationSetting,
    ) -> ClientResult<AgentView> {
        self.agent_view_request(Request::AgentInventoryMutate {
            agent,
            expected,
            mutation,
        })
        .await
    }

    async fn agent_view_request(&self, request: Request) -> ClientResult<AgentView> {
        match self.agent_request(request).await? {
            Response::Agent(agent) => Ok(*agent),
            other => Err(ClientError::internal(format!(
                "agent mutation returned {other:?}"
            ))),
        }
    }

    async fn agent_request(&self, request: Request) -> ClientResult<Response> {
        let daemon = self.daemon()?;
        let response = daemon
            .request(ControlRoute::Daemon, &request, None)
            .await
            .map_err(|error| {
                ClientError::unreachable(format!("reach agent inventory: {error:#}"))
            })?;
        match response {
            Response::Error { message, .. } => Err(ClientError::refused(message)),
            response => Ok(response),
        }
    }
}
