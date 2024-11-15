use crate::requests::EmailRequest;
use aws_config::meta::region::RegionProviderChain;
use aws_config::Region;
use aws_sdk_ses::types::Body as EmailBody;
use aws_sdk_ses::types::Content as EmailContent;
use aws_sdk_ses::types::Destination;
use aws_sdk_ses::types::Message as EmailMessage;
use aws_sdk_ses::Client;
use ginger_shared_rs::ISCClaims;

#[utoipa::path(
    post,
    path = "/notification/send-email",
    request_body = EmailRequest,
    responses(
        (status = 200, description = "Email sent"),
    ),
    security(("apiISCBearerAuth" = [])),  // Referencing the security scheme
    tag = "default"
)]
pub async fn send_email(
    email_request: EmailRequest,
    claims: ISCClaims, // Add claims from JWT here
) -> Result<impl warp::Reply, warp::Rejection> {
    println!("claims : {:?}", claims);
    // Set up AWS SES client
    let region_provider =
        RegionProviderChain::default_provider().or_else(Region::new("ap-south-1"));
    let config = aws_config::from_env().region(region_provider).load().await;
    let client = Client::new(&config);

    let destination = Destination::builder()
        .to_addresses(email_request.to.clone())
        .build();

    // Construct the Message object
    let message = EmailMessage::builder()
        .subject(
            EmailContent::builder()
                .data(email_request.subject.clone())
                .build()
                .unwrap(),
        )
        .body(
            EmailBody::builder()
                .text(
                    EmailContent::builder()
                        .data(email_request.message.clone())
                        .build()
                        .unwrap(),
                )
                .build(),
        )
        .build();

    // Prepare the SES email request
    let email_result = client
        .send_email()
        .source("no-reply@gingersociety.org") // Replace with SES verified email
        .destination(destination)
        .message(message)
        .send()
        .await;

    // Check for errors and send response
    match email_result {
        Ok(_) => Ok(warp::reply::json(&"Email sent")),
        Err(err) => {
            eprintln!("Failed to send email: {:?}", err);
            Ok(warp::reply::json(&"Failed to send email"))
        }
    }
}
