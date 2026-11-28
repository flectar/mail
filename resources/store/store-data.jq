# The demo data with an invented inbox and invented contacts, for the Google
# Play listing: Play's listing rules don't allow other brands' names or logos
# without permission. scripts/generate-store-assets.sh applies this to
# resources/screenshots/demo-data.json before scripts/prepare-screenshot-data.jq.
# Every address needs a picture in resources/store/senders, named after its
# domain. Accounts keep the demo names here, because the screenshot data maps
# them to account ids; store-view.jq renames them afterwards.
#
# The open message is the first row's, which the list screenshots show as
# selected, or with $open set, the row from that address, for the reader's.
def row($sender; $account; $initials; $address; $subject; $preview):
  . + {sender: $sender, account: $account, initials: $initials, address: $address,
       subject: $subject, preview: $preview,
       label_categories: [], label_summary: "", labels: []};
def mail_label($id; $name; $color):
  {id: $id, name: $name, name_has_emoji: false, color: $color, applied: true, is_auto: false};
def contact($name; $initials; $email; $company; $detail):
  . + {name: $name, initials: $initials, email: $email, company: $company, detail: $detail};
# The reader dates a message as the app does, today being Wednesday, 9 September.
def reader_time($list_time):
  {"Mon": "Sep 7", "Sun": "Sep 6", "Fri": "Sep 4", "Thu": "Sep 3", "Wed": "Sep 2"}[$list_time] // $list_time;

[
  ["Lena Ortiz", "Flectar", "LO", "lena@ortiz.example", "Lisbon trip: flights are booked", "Both legs are confirmed. Here's the plan for Saturday…"],
  ["Northwind Bank", "Flectar", "NB", "statements@northwind.example", "Your November statement is ready", "Your statement for November is now available…"],
  ["Maya Fischer", "Flectar Support", "MF", "maya@studio.example", "Invitation: Design review", "Thursday at 10:00 AM · Studio office"],
  ["Harbor Cloud", "Flectar Support", "HC", "security@harbor.example", "Your sign-in code", "Verification code: 482913. Use it to finish signing in…"],
  ["Sam Patel", "Flectar Support", "SP", "sam@patel.example", "Re: Q4 report draft", "Looks great. Two small notes on the budget section…"],
  ["Kite Airlines", "Flectar", "KA", "hello@kite.example", "Check-in is open for your flight", "Lisbon · Saturday at 7:05 AM · Seat 14A…"],
  ["Ava Chen", "Flectar", "AC", "ava@chen.example", "Photos from Saturday", "Here are the best ones from the beach…"],
  ["Maple & Co.", "Flectar", "MC", "orders@maple.example", "Your order has shipped", "It's on its way and should arrive by Tuesday…"],
  ["Jonas Berg", "Flectar", "JB", "jonas@berg.example", "Dinner on Friday?", "We found a new place near the river…"],
  ["Studio Weekly", "Flectar Support", "SW", "news@studio.example", "This week at the studio", "Three new projects, a team photo and…"],
  ["Priya Nair", "Flectar Support", "PN", "priya@nair.example", "Contract signed", "All set. I've attached the countersigned copy…"],
  ["City Library", "Flectar", "CL", "loans@library.example", "Your books are due next week", "Renew them online or at any branch…"]
] as $mail
| [
  ["Lena Ortiz", "LO", "lena@ortiz.example", "Ortiz Studio", "Ortiz Studio · Architect"],
  ["Maya Fischer", "MF", "maya@studio.example", "Studio", "Studio · Design lead"],
  ["Sam Patel", "SP", "sam@patel.example", "Northwind Bank", "Northwind Bank · Analyst"],
  ["Priya Nair", "PN", "priya@nair.example", "Harbor Cloud", "Harbor Cloud · Counsel"],
  ["Jonas Berg", "JB", "jonas@berg.example", "", "Photographer"],
  ["Ava Chen", "AC", "ava@chen.example", "", ""]
] as $people
| .emails |= [to_entries[] | .key as $i | .value
    | $mail[$i % ($mail | length)] as $m
    | row($m[0]; $m[1]; $m[2]; $m[3]; $m[4]; $m[5])
    | if $i == 0 or $i == 5 then .labels = [mail_label(1; "Travel"; "#4a86e8")] | .label_summary = "Travel" else . end
    | if $i == 6 then .labels = [mail_label(3; "Family"; "#16a765")] | .label_summary = "Family" else . end
    | if .attachment? then .attachment.stem = "Statement" else . end]
| .contacts |= [to_entries[] | .key as $i | .value
    | $people[$i % ($people | length)] as $p
    | contact($p[0]; $p[1]; $p[2]; $p[3]; $p[4])]
| .contact_name = "Lena Ortiz"
| .contact_initials = "LO"
| .contact_email = "lena@ortiz.example"
| .contact_phone = "+1 555 0142"
| .contact_company = "Ortiz Studio"
| .contact_job_title = "Architect"
| .contact_website = "https://ortiz.example"
| .contact_notes = "Planning the Lisbon trip together."
| .contact_tags = "friends, travel"
| (.emails | map(.address) | index($open) // 0) as $index
| .emails[$index] as $row
| .selected_sender = $row.sender
| .selected_address = $row.address
| .selected_initials = $row.initials
| .selected_subject = $row.subject
| .selected_time = reader_time($row.time)
| .selected_label = ($row.label_summary | ascii_upcase)
# The reader lists the account's labels and marks the open message's.
| ($row.labels | map(.id)) as $applied
| .mail_labels = ([mail_label(1; "Travel"; "#4a86e8"), mail_label(3; "Family"; "#16a765"),
    mail_label(4; "Follow up"; "#8b5cf6")] | map(.applied = ([.id] | inside($applied))))
| .selected_mail_label_count = ($applied | length)
| .thread_messages = []
