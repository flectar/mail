# Applied after scripts/prepare-screenshot-data.jq: the accounts get everyday
# names, every sender and contact shows its picture, and the layout is
# Android's. $senders is the folder of rasterized sender pictures.
def picture($address): $senders + "/" + ($address | split("@") | last) + ".png";

walk(if . == "Flectar Support" then "Work" elif . == "Flectar" then "Personal"
  elif . == "Support" then "Team" else . end)
| .emails |= map(.has_favicon = true)
| .mail_list_entries |= map(.email.has_favicon = true)
| .contacts |= map(.avatar = picture(.email) | .["has-avatar"] = true)
| .["contact-photo"] = picture(.["contact-emails"][0].value)
| .["contact-has-photo"] = true
| .["AppIcon.platform"] = "android"
