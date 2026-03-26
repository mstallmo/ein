use clap::Parser;
use ein_proto::ein::{RunAgentRequest, agent_client::AgentClient, agent_event::Event};
use tonic::transport::Channel;

#[derive(Parser)]
#[command(author, version, about)]
struct Args {
    #[arg(short = 'p', long)]
    prompt: String,

    #[arg(long, default_value = "http://localhost:50051")]
    server: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let channel = Channel::from_shared(args.server)?.connect().await?;
    let mut client = AgentClient::new(channel);

    let request = tonic::Request::new(RunAgentRequest {
        prompt: args.prompt,
    });

    let mut stream = client.run_agent(request).await?.into_inner();

    while let Some(event) = stream.message().await? {
        match event.event {
            Some(Event::ContentDelta(d)) => print!("{}", d.text),
            Some(Event::ToolCallStart(t)) => {
                eprintln!("[tool] {} args={}", t.tool_name, t.arguments)
            }
            Some(Event::ToolCallEnd(t)) => eprintln!("[tool] {} done", t.tool_name),
            Some(Event::AgentFinished(f)) => {
                println!("{}", f.final_content);
                break;
            }
            Some(Event::AgentError(e)) => {
                eprintln!("Error: {}", e.message);
                break;
            }
            None => {}
        }
    }

    Ok(())
}
